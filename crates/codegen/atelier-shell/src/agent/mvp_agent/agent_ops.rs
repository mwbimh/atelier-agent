#![cfg_attr(rustfmt, rustfmt::skip)]
#![allow(unused_imports)]
//! Inherent [`MvpAgent`] helpers (MCP/clients/gateway, settings/models, session ops, spawn).
//! Co-located child of `mvp_agent` (`use super::*`).
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionFilesystemMode {
    AcpClient,
    WorkspaceWorker,
    InProcessTest,
}

#[cfg(test)]
mod filesystem_selection_tests {
    use super::{SessionFilesystemMode, refreshed_provider_payload, session_filesystem_mode};

    #[test]
    fn acp_filesystem_is_selected_when_client_provides_it() {
        assert_eq!(
            session_filesystem_mode(true, false),
            SessionFilesystemMode::AcpClient
        );
    }

    #[test]
    fn local_production_sessions_require_the_worker() {
        assert_eq!(
            session_filesystem_mode(false, false),
            SessionFilesystemMode::WorkspaceWorker
        );
    }

    #[test]
    fn tests_may_explicitly_opt_into_in_process_filesystem() {
        assert_eq!(
            session_filesystem_mode(false, true),
            SessionFilesystemMode::InProcessTest
        );
    }

    #[test]
    fn provider_payload_refresh_preserves_captured_role_overlay() {
        let previous_provider = serde_json::json!({
            "provider_only": "old",
            "shared": "provider-old",
        })
        .as_object()
        .unwrap()
        .clone();
        let current_session = serde_json::json!({
            "provider_only": "old",
            "role_only": true,
            "shared": "role",
        })
        .as_object()
        .unwrap()
        .clone();
        let refreshed_provider = serde_json::json!({
            "provider_only": "new",
            "shared": "provider-new",
        })
        .as_object()
        .unwrap()
        .clone();

        let refreshed = refreshed_provider_payload(
            &current_session,
            &previous_provider,
            &refreshed_provider,
        );

        assert_eq!(refreshed["provider_only"], "new");
        assert_eq!(refreshed["role_only"], true);
        assert_eq!(refreshed["shared"], "role");
    }
}

fn session_filesystem_mode(use_acp_fs: bool, allow_in_process: bool) -> SessionFilesystemMode {
    if use_acp_fs {
        SessionFilesystemMode::AcpClient
    } else if allow_in_process {
        SessionFilesystemMode::InProcessTest
    } else {
        SessionFilesystemMode::WorkspaceWorker
    }
}

fn load_configured_role(
    path: &std::path::Path,
    role_id: atelier_provider::RoleId,
    main: &atelier_provider::RoleConfig,
    require_role: bool,
) -> Result<Option<atelier_provider::RoleConfig>, acp::Error> {
    atelier_provider::ProviderRegistry::load_or_create(path)
        .map(|registry| {
            let configured = registry.roles().find_inherited(role_id).is_some();
            (require_role || configured || role_id == atelier_provider::RoleId::Main)
                .then(|| registry.roles().resolve_inherited(role_id, main).1)
        })
        .map_err(|error| {
            acp::Error::invalid_params().data(format!(
                "failed to load configured {role_id} role: {error}"
            ))
        })
}

fn main_role_from_sampling_config(
    config: &SamplingConfig,
    model_id: &str,
) -> Result<atelier_provider::RoleConfig, acp::Error> {
    let (provider, model) = model_id.split_once('/').or_else(|| {
        config
            .provider_id
            .as_deref()
            .map(|provider| (provider, config.model.as_str()))
    })
    .ok_or_else(|| {
        acp::Error::invalid_params().data(format!(
            "MAIN model must use provider/model format: {model_id}"
        ))
    })?;
    let mut main = atelier_provider::RoleConfig::new(provider, model)
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    main.effort = config.reasoning_effort.map(|effort| effort.to_string());
    if let Some(fast_mode) = atelier_provider::fast_mode_from_payload(&config.request_payload) {
        main.set_fast_mode(fast_mode);
    }
    Ok(main)
}

const LIVE_PROVIDER_MODEL_UNAVAILABLE_PREFIX: &str = "__atelier_live_provider_unavailable__:";

fn live_provider_model_unavailable_latch(model_id: &acp::ModelId) -> acp::ModelId {
    acp::ModelId::new(format!(
        "{LIVE_PROVIDER_MODEL_UNAVAILABLE_PREFIX}{}",
        model_id.0
    ))
}

fn refreshed_provider_payload(
    current_session_payload: &serde_json::Map<String, serde_json::Value>,
    previous_provider_payload: &serde_json::Map<String, serde_json::Value>,
    refreshed_provider_payload: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let preserved_role_overlay = current_session_payload
        .iter()
        .filter(|(key, value)| previous_provider_payload.get(*key) != Some(*value))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    atelier_provider::merge_payloads(refreshed_provider_payload, &preserved_role_overlay)
}

impl MvpAgent {
    /// Reload the local Provider catalog and reconcile every resident session
    /// against the new immutable model snapshot before the extension response
    /// is returned to the client.
    pub(crate) async fn reload_local_provider_catalog_and_reconcile_sessions(
        &self,
    ) -> Result<(), String> {
        let path = atelier_config::atelier_home().join("providers.toml");
        self.reload_local_provider_catalog_and_reconcile_sessions_from(&path)
            .await
    }

    pub(crate) async fn reload_local_provider_catalog_and_reconcile_sessions_from(
        &self,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let previous_models = self.models_manager.models();
        self.models_manager.reload_local_provider_catalog_from(path)?;
        self.reconcile_live_provider_sessions_from(previous_models).await;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn reconcile_live_provider_sessions(&self) {
        let previous_models = self.models_manager.models();
        self.reconcile_live_provider_sessions_from(previous_models).await;
    }

    async fn reconcile_live_provider_sessions_from(
        &self,
        previous_models: IndexMap<String, ModelEntry>,
    ) {
        let refreshed_models = self.models_manager.models();
        let available_models = self.models_manager.available();
        let sessions: Vec<_> = self
            .sessions
            .borrow()
            .iter()
            .map(|(session_id, handle)| (session_id.clone(), handle.clone()))
            .collect();

        for (session_id, handle) in sessions {
            let catalog_key = resolve_catalog_key(&refreshed_models, &handle.model_id);
            let refreshed_entry = catalog_key
                .as_ref()
                .filter(|key| available_models.contains_key(*key))
                .and_then(|key| refreshed_models.get(key.0.as_ref()));

            let Some(refreshed_entry) = refreshed_entry else {
                let latch = live_provider_model_unavailable_latch(&handle.model_id);
                let changed = self
                    .model_unavailable_sessions
                    .borrow_mut()
                    .insert(session_id.0.to_string(), latch)
                    .is_none_or(|previous| {
                        !previous
                            .0
                            .starts_with(LIVE_PROVIDER_MODEL_UNAVAILABLE_PREFIX)
                    });
                if changed {
                    tracing::warn!(
                        session_id = %session_id.0,
                        model_id = %handle.model_id.0,
                        "Provider catalog reload made the active session model unavailable"
                    );
                    self.notify_live_provider_model_unavailable(&session_id, &handle.model_id);
                }
                continue;
            };

            if self
                .model_unavailable_sessions
                .borrow()
                .contains_key(session_id.0.as_ref())
            {
                continue;
            }

            let previous_entry = resolve_catalog_key(&previous_models, &handle.model_id)
                .as_ref()
                .and_then(|key| previous_models.get(key.0.as_ref()));
            let (payload_tx, payload_rx) = oneshot::channel();
            let current_payload = if handle
                .cmd_tx
                .send(SessionCommand::GetRequestPayload {
                    responds_to: payload_tx,
                })
                .is_ok()
            {
                payload_rx.await.unwrap_or_default()
            } else {
                tracing::warn!(
                    session_id = %session_id.0,
                    "Provider catalog reload could not query the resident session payload"
                );
                continue;
            };

            let mut sampling_config =
                self.prepare_sampling_config_for_model(refreshed_entry, handle.origin_client.clone());
            sampling_config.reasoning_effort = handle.reasoning_effort;
            let empty_previous_payload = serde_json::Map::new();
            sampling_config.request_payload = refreshed_provider_payload(
                &current_payload,
                previous_entry
                    .map(|entry| &entry.request_payload)
                    .unwrap_or(&empty_previous_payload),
                &sampling_config.request_payload,
            );
            let auto_compact_threshold_percent = {
                let cfg = self.cfg.borrow();
                crate::util::config::resolve_auto_compact_threshold_percent(
                    &cfg,
                    catalog_key
                        .as_ref()
                        .map(|key| key.0.as_ref())
                        .unwrap_or(handle.model_id.0.as_ref()),
                    Some(&refreshed_entry.info),
                )
            };
            let (responds_to, response) = oneshot::channel();
            if handle
                .cmd_tx
                .send(SessionCommand::RefreshProviderModel {
                    sampling_config,
                    auto_compact_threshold_percent,
                    responds_to,
                })
                .is_err()
            {
                tracing::warn!(
                    session_id = %session_id.0,
                    "Provider catalog reload could not refresh the resident session sampler"
                );
                continue;
            }
            match response.await {
                Ok(Ok(_)) => {
                    self.notify_live_provider_model_refreshed(
                        &session_id,
                        &handle.model_id,
                        handle.reasoning_effort,
                    );
                }
                Ok(Err(error)) => tracing::warn!(
                    session_id = %session_id.0,
                    error = ?error,
                    "Provider catalog reload session sampler refresh failed"
                ),
                Err(_) => tracing::warn!(
                    session_id = %session_id.0,
                    "Provider catalog reload session sampler refresh response was dropped"
                ),
            }
        }
    }

    fn notify_live_provider_model_unavailable(
        &self,
        session_id: &acp::SessionId,
        model_id: &acp::ModelId,
    ) {
        let notification = SessionNotification {
            session_id: session_id.clone(),
            update: SessionUpdate::ModelAutoSwitched {
                previous_model_id: model_id.0.to_string(),
                new_model_id: String::new(),
                reason: format!(
                    "Model \"{}\" is unavailable. Select a model before sending the next prompt.",
                    model_id.0
                ),
            },
            meta: None,
        };
        if let Ok(params) = serde_json::value::to_raw_value(&notification) {
            self.gateway.forward_fire_and_forget(acp::ExtNotification::new(
                "atelier/session_notification",
                params.into(),
            ));
        }
    }

    fn notify_live_provider_model_refreshed(
        &self,
        session_id: &acp::SessionId,
        model_id: &acp::ModelId,
        reasoning_effort: Option<atelier_sampling_types::ReasoningEffort>,
    ) {
        let notification = SessionNotification {
            session_id: session_id.clone(),
            update: SessionUpdate::ModelChanged {
                model_id: model_id.0.to_string(),
                reasoning_effort: reasoning_effort.map(|effort| effort.to_string()),
            },
            meta: None,
        };
        if let Ok(params) = serde_json::value::to_raw_value(&notification) {
            self.gateway.forward_fire_and_forget(acp::ExtNotification::new(
                "atelier/session_notification",
                params.into(),
            ));
        }
    }

    pub(super) fn resolve_image_description_model(&self) -> String {
        self.cfg
            .borrow()
            .image_description_model
            .as_deref()
            .unwrap_or(crate::models::default_image_description_model())
            .to_owned()
    }
    /// Read one of the fixed Provider Roles. Missing entries are intentionally
    /// absent; ordinary main turns and unconfigured subagent Roles may inherit
    /// the active model, while explicitly Role-owned helper flows can use
    /// [`Self::required_role`] to require a configured assignment.
    fn current_main_role_config(&self) -> Result<atelier_provider::RoleConfig, acp::Error> {
        let model_id = self.models_manager.current_model_id();
        let entry = self.resolve_model_id(&model_id).map_err(|_| {
            acp::Error::invalid_params().data(format!(
                "MAIN model is unavailable: {}",
                model_id.0
            ))
        })?;
        let (provider, model) = model_id.0.split_once('/').ok_or_else(|| {
            acp::Error::invalid_params().data(format!(
                "MAIN model must use provider/model format: {}",
                model_id.0
            ))
        })?;
        let mut main = atelier_provider::RoleConfig::new(provider, model)
            .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
        main.effort = self
            .models_manager
            .current_reasoning_effort()
            .map(|effort| effort.to_string());
        if let Some(fast_mode) = atelier_provider::fast_mode_from_payload(&entry.request_payload) {
            main.set_fast_mode(fast_mode);
        }
        Ok(main)
    }

    pub(crate) fn configured_role(
        &self,
        role_id: atelier_provider::RoleId,
    ) -> Result<Option<atelier_provider::RoleConfig>, acp::Error> {
        let path = atelier_config::atelier_home().join("providers.toml");
        let main = self.current_main_role_config()?;
        load_configured_role(&path, role_id, &main, false)
    }

    pub(crate) fn required_role(
        &self,
        role_id: atelier_provider::RoleId,
    ) -> Result<atelier_provider::RoleConfig, acp::Error> {
        let path = atelier_config::atelier_home().join("providers.toml");
        let main = self.current_main_role_config()?;
        load_configured_role(&path, role_id, &main, true)?.ok_or_else(|| {
            acp::Error::invalid_params().data(format!("role {role_id} could not be resolved"))
        })
    }

    /// Apply a captured fixed Role to the session's sampler configuration.
    ///
    /// The role payload is copied into the SessionActor snapshot and the
    /// common effort setting is parsed once at spawn time. Provider/model
    /// selection is resolved by the caller before this helper runs.
    fn apply_role_to_sampling_config(
        config: &mut SamplingConfig,
        role_id: atelier_provider::RoleId,
        role: &atelier_provider::RoleConfig,
    ) -> Result<(), acp::Error> {
        config.request_payload = role.merged_payload(&config.request_payload);
        if let Some(effort) = role.effort.as_deref() {
            config.reasoning_effort = Some(effort.parse().map_err(|_| {
                acp::Error::invalid_params().data(format!(
                    "unsupported {role_id} role effort: {effort}"
                ))
            })?);
        }
        Ok(())
    }

    pub(super) fn build_summary_client(
        &self,
        primary: &SamplingConfig,
    ) -> Result<(OaiCompatClient, String), acp::Error> {
        let role_registry = atelier_provider::ProviderRegistry::load_or_create(
            atelier_config::atelier_home().join("providers.toml"),
        )
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
        let main_role = main_role_from_sampling_config(primary, &primary.model)?;
        let title_role = role_registry
            .roles()
            .find_inherited(atelier_provider::RoleId::Title)
            .map(|_| {
                role_registry
                    .roles()
                    .resolve_inherited(atelier_provider::RoleId::Title, &main_role)
                    .1
            });
        let policy_provider = title_role
            .as_ref()
            .map(|role| role.provider.as_str())
            .or(primary.provider_id.as_deref());
        let policy_decision = crate::extensions::policy::evaluate_runtime_policy(
            &self.policy_engine.read(),
            crate::extensions::policy::PolicyOperation::ProviderRequest,
            Some(atelier_provider::RoleId::Title.as_str()),
            policy_provider,
            None,
            None,
            crate::extensions::policy::PolicyGates::default(),
        );
        match policy_decision {
            atelier_hooks::PolicyDecision::Allow => {}
            atelier_hooks::PolicyDecision::Deny { reason } => {
                return Err(acp::Error::invalid_params().data(reason));
            }
            atelier_hooks::PolicyDecision::Ask { prompt } => {
                return Err(acp::Error::invalid_params()
                    .data(format!("Provider request requires approval: {prompt}")));
            }
            atelier_hooks::PolicyDecision::Modify { .. }
            | atelier_hooks::PolicyDecision::AddContext { .. } => {
                return Err(acp::Error::invalid_params().data(
                    "Title Provider policy requested an unsupported request mutation",
                ));
            }
        }
        let Some(title_role) = title_role else {
            let config = primary.clone();
            let model = config.model.clone();
            let client = OaiCompatClient::new(config).map_err(map_sampling_err_to_acp)?;
            return Ok((client, model));
        };
        let slug = format!("{}/{}", title_role.provider, title_role.model);
        let session_key = self.auth_manager.current_or_expired().map(|a| a.key.clone());
        let models = self.models_manager.models();
        let endpoints = self.models_manager.endpoints();
        let (disable_api_key_auth, alpha_test_key, client_version) = {
            let cfg = self.cfg.borrow();
            (
                cfg.atelier_com_config.api_key_auth_disabled(),
                cfg.endpoints.alpha_test_key.clone(),
                cfg.client_version.clone(),
            )
        };
        let mut config = if title_role.provider == main_role.provider
            && title_role.model == main_role.model
        {
            primary.clone()
        } else {
            match crate::agent::config::resolve_aux_model_sampling_config(
                &slug,
                &models,
                &endpoints,
                session_key.as_deref(),
                disable_api_key_auth,
                alpha_test_key,
                client_version,
            ) {
                Some(mut cfg) => {
                    cfg.client_identifier = primary.client_identifier.clone();
                    cfg.attribution_callback = primary.attribution_callback.clone();
                    cfg.bearer_resolver = primary.bearer_resolver.clone();
                    cfg.max_retries = primary.max_retries;
                    cfg
                }
                None => {
                    return Err(acp::Error::invalid_params().data(format!(
                        "configured title role model is unavailable: {slug}"
                    )));
                }
            }
        };
        config.request_payload = title_role.merged_payload(&config.request_payload);
        if let Some(raw_effort) = title_role.effort.as_deref() {
            config.reasoning_effort = Some(raw_effort.parse().map_err(|_| {
                acp::Error::invalid_params().data(format!(
                    "unsupported title role effort: {raw_effort}"
                ))
            })?);
        }
        let model = config.model.clone();
        let client = OaiCompatClient::new(config).map_err(map_sampling_err_to_acp)?;
        Ok((client, model))
    }
    fn has_proxy_credentials(&self) -> bool {
        self.cfg.borrow().endpoints.deployment_key.is_some()
            || self.auth_manager.current_or_expired().is_some_and(|a| a.is_configured_refresh_auth())
    }
    /// `true` for session-based ACP auth methods.
    fn is_session_based_auth(&self) -> bool {
        self.auth_method_id
            .load()
            .as_deref()
            .is_some_and(crate::agent::auth_method::is_session_based_method)
    }
    /// Publish the current ACP auth method into the shared live handle so every
    /// running session's per-turn auth gate observes it on its next turn.
    pub(super) fn set_auth_method(&self, id: acp::AuthMethodId) {
        self.auth_method_id.store(Some(std::sync::Arc::new(id)));
    }
    /// Return auth for sync config construction.
    pub(super) fn current_or_buffered_auth(&self) -> Option<crate::auth::AtelierAuth> {
        self.auth_manager
            .current()
            .or_else(|| {
                if self.is_session_based_auth() {
                    let auth = self.auth_manager.expired_auth();
                    if auth.is_some() {
                        atelier_telemetry::unified_log::info(
                            "auth buffered token fallback",
                            None,
                            None,
                        );
                    }
                    auth
                } else {
                    None
                }
            })
    }
    pub fn managed_mcp_cache(
        &self,
    ) -> &crate::session::managed_mcp::ManagedMcpStateHandle {
        &self.managed_mcp_cache
    }
    pub(crate) fn disable_managed_gateway_tools_and_refresh_sessions(&self) {
        self.disable_managed_gateway_tools_and_refresh_sessions_with_txs(
            self.sessions.borrow().values().map(|handle| handle.cmd_tx.clone()).collect(),
        );
    }
    fn disable_managed_gateway_tools_and_refresh_sessions_with_txs(
        &self,
        session_txs: Vec<tokio::sync::mpsc::UnboundedSender<SessionCommand>>,
    ) {
        let cache = self.managed_mcp_cache.clone();
        tokio::task::spawn_local(async move {
            cache.lock().await.disable_gateway_tools();
            for tx in session_txs {
                let _ = tx.send(SessionCommand::RefreshMcpSearchIndex);
            }
        });
    }
    /// Resolve the launch dir's project-scope trust verdict ONCE and return it
    /// with its path.
    ///
    /// Memoizes the single [`folder_trust::resolve_launch_dir_trust`] gather (see
    /// it for the dedup + TOCTOU contract) so the two one-shot init helpers
    /// (`ensure_plugin_registry` and `ensure_local_workspace_ops`) share it
    /// instead of each re-scanning. They share a single point-in-time verdict
    /// rather than two independent re-scans; the sub-millisecond, startup-only
    /// window between them is intentional (the cross-session TOCTOU re-scan is
    /// preserved per the contract).
    fn prime_launch_dir_trust(&self) -> (&std::path::Path, bool) {
        let trust = *self
            .launch_dir_trust
            .get_or_init(|| {
                let local_runtime_settings = self.cfg.borrow().local_runtime_settings.clone();
                folder_trust::resolve_launch_dir_trust(
                    &self.launch_cwd,
                    local_runtime_settings.as_ref(),
                )
            });
        (&self.launch_cwd, trust)
    }
    /// Resolve folder trust and load launch-dir MCP configs after `initialize`
    /// returns. The walks are synchronous and expensive in large monorepos; they
    /// must not block the ACP response (atelier-desktop sends `initialize` immediately).
    pub(super) fn spawn_initialize_launch_mcp_setup(&self) {
        let cwd = self.launch_cwd.clone();
        let compat = self.cfg.borrow().compat_resolved;
        let local_runtime_settings = self.cfg.borrow().local_runtime_settings.clone();
        let gateway = self.gateway.clone();
        let agent_mcp_state = self.agent_mcp_state.clone();
        tokio::task::spawn_local(async move {
            let local_mcp_servers = match tokio::task::spawn_blocking(move || {
                    let local = crate::util::config::load_mcp_servers(&cwd, &compat);
                    folder_trust::resolve_and_record(
                        &cwd,
                        local_runtime_settings.as_ref(),
                        false,
                    );
                    folder_trust::filter_untrusted_project_mcp(&cwd, local)
                })
                .await
            {
                Ok(servers) => servers,
                Err(e) => {
                    tracing::warn!(error = % e, "initialize MCP setup task failed");
                    return;
                }
            };
            if !local_mcp_servers.is_empty() {
                agent_mcp_state.lock().await.update_configs(local_mcp_servers.clone());
            }
            crate::extensions::mcp::notify_servers_updated(
                    &gateway,
                    &[],
                    &local_mcp_servers,
                )
                .await;
        });
    }
    pub fn agent_mcp_state(
        &self,
    ) -> std::sync::Arc<tokio::sync::Mutex<crate::session::mcp_servers::McpState>> {
        self.agent_mcp_state.clone()
    }
    /// Build the launch-dir plugin registry snapshot on first use.
    ///
    /// Boot-time discovery was deferred past ACP `initialize` (the cwd→git-root
    /// plus user/marketplace walks stalled atelier-desktop's first `initialize`),
    /// leaving `plugin_registry_handle` empty. That shared snapshot still backs
    /// the launch-dir plugin MCP/LSP merges read in `resolve_mcp_servers` and
    /// the session LSP build, so populate it lazily — off the `initialize`
    /// critical path — on the first session-creating call. Runs the discovery
    /// walk once; per-session `build_for_cwd` still re-resolves project-scoped
    /// plugins for each session's own cwd.
    pub(super) fn ensure_plugin_registry(&self) {
        if self.plugin_registry_initialized.replace(true) {
            return;
        }
        let (cwd, trusted) = self.prime_launch_dir_trust();
        let mut plugins = self.cfg.borrow().plugins.clone();
        plugins.merge_claude_enabled_plugins(Some(cwd));
        let disk_config = plugins.to_discovery_config();
        let count = self
            .plugin_registry_handle
            .reload(Some(cwd), &disk_config, trusted, false);
        tracing::debug!(
            plugin_count = count, "lazily populated plugin registry snapshot"
        );
    }
    /// Merge client-provided servers with trusted local and plugin MCP configs.
    pub(super) async fn resolve_mcp_servers(
        &self,
        client_servers: Vec<acp::McpServer>,
        cwd: &std::path::Path,
    ) -> (Vec<acp::McpServer>, Option<chrono::DateTime<chrono::Utc>>) {
        self.ensure_plugin_registry();
        let merged = crate::session::managed_mcp::merge_managed_mcp_servers(
            client_servers,
            cwd,
            &[],
            self.plugin_registry_handle.snapshot().as_deref(),
            &self.cfg.borrow().compat_resolved,
        );
        (merged, None)
    }
    /// Set the memory configuration (called from TUI after config resolution).
    pub fn set_memory_config(&mut self, config: crate::config::MemoryConfig) {
        self.memory_config = if config.enabled { Some(config) } else { None };
    }
    /// Adopt the leader's [`AgentActivity`] so an explicit relaunch drain sees
    /// the agent's live view of running turns/subagents and can flush sessions
    /// at shutdown.
    ///
    /// Must be called right after construction: entries registered on the
    /// constructor-created default instance are NOT migrated.
    pub fn set_activity(&mut self, activity: crate::agent::activity::AgentActivity) {
        self.subagent_coordinator
            .borrow_mut()
            .set_running_gauge(activity.subagent_gauge());
        self.activity = activity;
    }
    /// Install the channel that fans new session cwds into the leader's
    /// `ConfigFileWatcher::watch_path`. Called once after
    /// the watcher is constructed in `agent/app.rs`. In simple /
    /// non-leader mode the channel is never wired and
    /// `notify_session_cwd_for_watch` is a no-op.
    pub fn set_config_watcher_path_tx(
        &mut self,
        tx: tokio::sync::mpsc::UnboundedSender<std::path::PathBuf>,
    ) {
        self.config_watcher_path_tx = Some(tx);
    }
    /// Best-effort fan-out of a new session's `cwd` to the leader's
    /// `ConfigFileWatcher` for dynamic non-recursive registration
    /// No-op if the channel was never installed
    /// (`set_config_watcher_path_tx` was not called — simple mode,
    /// tests) or if the receiver has been dropped. Watcher errors are
    /// logged inside the spawned task and do NOT propagate here.
    pub(crate) fn notify_session_cwd_for_watch(&self, cwd: &std::path::Path) {
        if let Some(tx) = self.config_watcher_path_tx.as_ref()
            && tx.send(cwd.to_path_buf()).is_err()
        {
            tracing::debug!(
                cwd = % cwd.display(),
                "config watcher path channel closed; session cwd not registered"
            );
        }
    }
    pub(super) fn ensure_telemetry_client(&self) {
        // Local observability is initialized by the process composition root.
    }
    pub(crate) fn local_session_catalog(
        &self,
    ) -> Option<crate::agent::local_session_catalog::LocalSessionCatalog> {
        None
    }
    /// Pre-session command availability snapshot.
    ///
    /// Used by the `atelier/commands/list` ext method and the
    /// `InitializeResponse._meta` path (`builtin_commands()`), both of
    /// which fire before any session exists. The eventual agent's toolset
    /// is unknown (depends on the model the user picks), so we fail-closed
    /// for runtime/tool-dependent gates (`/flush`, `/loop`, `/memory`,
    /// …) and let the session-scoped `available_commands_update` in
    /// `acp_session.rs` fill in the real per-model gating as soon as a
    /// session starts.
    ///
    /// Exception: `/goal` is gated on the `resolve_goal()` feature flag
    /// (a config/managed-settings switch known at initialize time) plus
    /// the `update_goal` tool, which is part of the default coding-agent
    /// toolset. So when the flag is on we advertise `/goal` pre-session;
    /// otherwise it wouldn't appear in the slash menu until after the
    /// first user turn created a session.
    pub(crate) fn command_availability(
        &self,
    ) -> crate::session::slash_commands::CommandAvailability {
        crate::session::slash_commands::CommandAvailability {
            goal: self.cfg.borrow().resolve_goal().value,
            ..crate::session::slash_commands::CommandAvailability::default()
        }
    }
    /// `true` when data collection should be suppressed (team ZDR or
    /// coding-data-retention opt-out). Delegates to
    /// [`AuthManager::is_data_collection_disabled`].
    pub(crate) fn is_data_collection_disabled(&self) -> bool {
        self.auth_manager.is_data_collection_disabled()
    }
    /// Telemetry enabled and not ZDR. Same gate as session `telemetry_enabled`.
    pub(crate) fn product_analytics_enabled(&self) -> bool {
        self.cfg.borrow().is_telemetry_enabled()
            && !self.auth_manager.current_or_expired().is_some_and(|a| a.is_zdr_team())
    }
    /// Current client type as set by the most recent `initialize()` call.
    pub(crate) fn client_type(&self) -> ClientType {
        *self.client_type.borrow()
    }
    /// Most recently allocated turn number for `sid`, or `None` if the
    /// session has not started a turn yet.
    pub(crate) fn session_turn_number(&self, sid: &acp::SessionId) -> Option<u64> {
        self.session_turn_numbers.borrow().get(sid).copied()
    }
    /// Return the current AtelierAuth credentials, if authenticated and not expired.
    pub(crate) fn current_auth(&self) -> Option<crate::auth::AtelierAuth> {
        self.auth_manager.current()
    }
    /// Shared plugin registry handle used by extensions for snapshot/reload.
    pub(crate) fn plugin_registry_handle(
        &self,
    ) -> &atelier_agent::plugins::SharedPluginRegistryHandle {
        &self.plugin_registry_handle
    }
    /// `true` when the agent runs in writeback storage mode.
    pub(crate) fn is_writeback_storage(&self) -> bool {
        false
    }
    /// Resolved cli-chat-proxy base for session features (via
    /// `proxy_url`). Not for the deployment-config fetch.
    pub(crate) fn cli_chat_proxy_base_url(&self) -> String {
        self.cfg.borrow().endpoints.proxy_url()
    }
    pub(crate) fn alpha_test_key(&self) -> Option<String> {
        self.cfg.borrow().endpoints.alpha_test_key.clone()
    }
    /// Build the process-lifetime local `WorkspaceOps` on first use.
    ///
    /// Deferred past ACP wiring so `initialize` can respond before folder-trust
    /// scans and `WorkspaceHandle::new_minimal` run (same boot stall as plugin
    /// discovery on atelier-desktop Windows).
    fn ensure_local_workspace_ops(
        &self,
    ) -> Result<atelier_workspace::WorkspaceOps, acp::Error> {
        if let Some(ops) = self.workspace_ops.borrow().clone() {
            return Ok(ops);
        }
        let (cwd, project_lsp_trusted) = self.prime_launch_dir_trust();
        let workspace_identity = self
            .auth_manager
            .current_or_expired()
            .map(|a| match a.team_id.filter(|t| !t.is_empty()) {
                Some(team) => {
                    atelier_workspace::WorkspaceIdentity::team(a.user_id, team)
                }
                None => {
                    atelier_workspace::WorkspaceIdentity::new(
                        a.user_id,
                        a.principal_type,
                        a.principal_id,
                    )
                }
            })
            .unwrap_or_default();
        let ops = match atelier_workspace::handle::WorkspaceHandle::new_minimal(
            cwd.to_path_buf(),
            workspace_identity,
            project_lsp_trusted,
        ) {
            Ok(handle) => atelier_workspace::WorkspaceOps::local(handle),
            Err(e) => {
                tracing::error!(error = % e, "failed to create local WorkspaceHandle");
                return Err(
                    acp::Error::internal_error().data("workspace not initialized"),
                );
            }
        };
        *self.workspace_ops.borrow_mut() = Some(ops.clone());
        Ok(ops)
    }

    pub(crate) fn resolve_session_workspace_ops(
        &self,
        cwd: &std::path::Path,
    ) -> Result<atelier_workspace::WorkspaceOps, acp::Error> {
        let key = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let launch_key = std::fs::canonicalize(&self.launch_cwd)
            .unwrap_or_else(|_| self.launch_cwd.clone());
        if key == launch_key {
            return self.resolve_workspace_ops();
        }
        if let Some(ops) = self.session_workspace_ops.borrow().get(&key).cloned() {
            return Ok(ops);
        }

        let workspace_identity = self
            .auth_manager
            .current_or_expired()
            .map(|auth| match auth.team_id.filter(|team| !team.is_empty()) {
                Some(team) => atelier_workspace::WorkspaceIdentity::team(auth.user_id, team),
                None => atelier_workspace::WorkspaceIdentity::new(
                    auth.user_id,
                    auth.principal_type,
                    auth.principal_id,
                ),
            })
            .unwrap_or_default();
        let project_lsp_trusted = folder_trust::project_scope_allowed(&key);
        let handle = atelier_workspace::handle::WorkspaceHandle::new_minimal(
            key.clone(),
            workspace_identity,
            project_lsp_trusted,
        )
        .map_err(|error| {
            tracing::error!(%error, cwd = %key.display(), "failed to create session workspace");
            acp::Error::internal_error().data("session workspace not initialized")
        })?;
        let ops = atelier_workspace::WorkspaceOps::local(handle);
        self.session_workspace_ops
            .borrow_mut()
            .insert(key, ops.clone());
        Ok(ops)
    }
    /// Resolve the workspace ops, returning `Err` if not yet initialized.
    ///
    /// Only `None` before the first lazy local build via
    /// [`Self::ensure_local_workspace_ops`]. Called at the `ext_method`
    /// dispatch boundary and in session spawn; extensions receive the
    /// resolved `&WorkspaceOps` directly.
    pub(crate) fn resolve_workspace_ops(
        &self,
    ) -> Result<atelier_workspace::WorkspaceOps, acp::Error> {
        let ops = self.ensure_local_workspace_ops()?;
        if let Some(handle) = ops.workspace_handle() && !handle.has_client_ext_sink() {
            let gw = self.gateway.clone();
            handle
                .set_client_ext_sink(
                    std::sync::Arc::new(move |method: String, params: serde_json::Value| {
                        if let Ok(raw) = serde_json::value::to_raw_value(&params) {
                            gw.forward_fire_and_forget(
                                acp::ExtNotification::new(method, raw.into()),
                            );
                        }
                    }),
                );
        }
        Ok(ops)
    }
    /// Derive the current `AuthType` from auth method + auth manager state.
    ///
    /// Conceptually, `AuthType` describes *which authentication mechanism this
    /// session uses*, not *whether we currently have a live bearer*. Bearer
    /// liveness is tracked by the auth manager; the mechanism is fixed by
    /// `auth_method_id`.
    ///
    /// Returns `SessionToken` when EITHER:
    ///   - `auth_manager` currently has a live (non-expired) credential, OR
    ///   - the active auth method is session-based (`cached_token`,
    ///     `atelier.invalid`, `oidc`) -- even if the in-memory token is currently
    ///     expired or missing.
    ///
    /// Returns `ApiKey` only when the auth method is BYOK (`xai.api_key`) or
    ///   no auth method has been selected yet AND no live credential exists.
    ///
    /// The session-based clause is load-bearing: without it, chat_state can get
    /// locked into `auth_type = ApiKey` and skip token refresh on later prompts.
    pub(crate) fn auth_type(&self) -> atelier_chat_state::AuthType {
        if self.auth_manager.current().is_some() || self.is_session_based_auth() {
            atelier_chat_state::AuthType::SessionToken
        } else {
            atelier_chat_state::AuthType::ApiKey
        }
    }
    /// When `cached_token` cannot proceed, prefer non-interactive Provider credentials;
    /// otherwise use the configured interactive method. Returns `None`
    /// when `preferred_method` is pinned (fail-closed — no cross-method fallthrough).
    pub(super) fn cached_token_fallthrough_method_id(
        &self,
    ) -> Option<acp::AuthMethodId> {
        let preferred = self.cfg.borrow().atelier_com_config.preferred_method;
        let id = auth_method::method_id_after_cached_token_unavailable(
            auth_method::should_advertise_provider_api_key(
                self.cfg.borrow().atelier_com_config.api_key_auth_disabled(),
                self.models_manager.models().values(),
            ),
            preferred,
        )?;
        Some(acp::AuthMethodId::new(id))
    }
    /// Shared exit for missing/expired/legacy `cached_token`: fall through with
    /// `use_oauth` only when the target is interactive `atelier.invalid`. When
    /// `preferred_method` is pinned, fail instead of falling through.
    pub(super) async fn authenticate_after_cached_token_unavailable(
        &self,
        arguments: acp::AuthenticateRequest,
    ) -> Result<AuthenticateResponse, acp::Error> {
        let Some(method_id) = self.cached_token_fallthrough_method_id() else {
            let preferred = self.cfg.borrow().atelier_com_config.preferred_method;
            let msg = match preferred {
                Some(crate::auth::PreferredAuthMethod::ApiKey) => {
                    auth_method::PREFERRED_API_KEY_UNAVAILABLE
                }
                _ => auth_method::PREFERRED_OIDC_UNAVAILABLE,
            };
            tracing::info!(
                % msg, "cached_token unavailable; preferred_method forbids fallthrough"
            );
            atelier_telemetry::unified_log::warn(
                "auth cached_token fallthrough blocked by preferred_method",
                None,
                Some(
                    serde_json::json!(
                        { "preferred_method" : preferred.map(| p | format!("{p:?}")), }
                    ),
                ),
            );
            return Err(acp::Error::auth_required().data(msg));
        };
        let meta = if method_id.0.as_ref() == auth_method::ATELIER_COM_METHOD_ID {
            serde_json::json!({ "use_oauth" : true }).as_object().cloned()
        } else {
            arguments.meta
        };
        tracing::info!(fallback = % method_id.0, "cached_token fallthrough");
        atelier_telemetry::unified_log::warn(
            "auth cached_token fallthrough",
            None,
            Some(serde_json::json!({ "fallback" : method_id.0.as_ref() })),
        );
        acp::Agent::authenticate(
                self,
                acp::AuthenticateRequest::new(method_id).meta(meta),
            )
            .await
    }
    pub(crate) fn deployment_key(&self) -> Option<String> {
        self.cfg.borrow().endpoints.deployment_key.clone()
    }
    pub(super) async fn send_model_auto_switched(
        &self,
        session_id: &acp::SessionId,
        previous: &acp::ModelId,
        new: &acp::ModelId,
        reason: &str,
    ) {
        let notification = crate::extensions::notification::SessionNotification {
            session_id: session_id.clone(),
            update: crate::extensions::notification::SessionUpdate::ModelAutoSwitched {
                previous_model_id: previous.0.to_string(),
                new_model_id: new.0.to_string(),
                reason: reason.to_string(),
            },
            meta: None,
        };
        if let Ok(params) = serde_json::value::to_raw_value(&notification) {
            let _ = self
                .gateway
                .ext_notification(
                    acp::ExtNotification::new("atelier/session_notification", params.into()),
                )
                .await;
        }
    }
    /// Pure id → entry resolver (the `allowed_models` gate lives in `set_session_model`).
    pub(crate) fn resolve_model_id(
        &self,
        requested: &acp::ModelId,
    ) -> Result<ModelEntry, acp::Error> {
        let requested_str = requested.0.as_ref();
        let models = self.models_manager.models();
        let Some(catalog_key) = resolve_catalog_key(&models, requested) else {
            tracing::debug!(
                requested = % requested_str, model_count = models.len(),
                "resolve_model_id: unknown model id (not in models() by key or .model field)"
            );
            return Err(acp::Error::invalid_params().data("unknown model id"));
        };
        let entry = models
            .get(catalog_key.0.as_ref())
            .expect("resolve_catalog_key returns a key present in models");
        let match_kind = if catalog_key.0.as_ref() == requested_str {
            "map key"
        } else {
            "model field scan"
        };
        tracing::debug!(
            "resolve_model_id: matched by {}: requested={} model={}", match_kind,
            requested_str, entry.info.model
        );
        Ok(entry.clone())
    }
    pub(crate) fn prepare_sampling_config_for_model(
        &self,
        model: &ModelEntry,
        origin_client: Option<crate::http::OriginClientInfo>,
    ) -> SamplingConfig {
        let preferred = self.cfg.borrow().atelier_com_config.preferred_method;
        let session = match preferred {
            Some(crate::auth::PreferredAuthMethod::ApiKey) => None,
            _ if self.is_session_based_auth() => self.auth_manager.current_or_expired(),
            _ => None,
        };
        let has_session_key = session.is_some();
        let mut credentials = resolve_credentials(
            model,
            session.as_ref().map(|a| a.key.as_str()),
        );
        if matches!(preferred, Some(crate ::auth::PreferredAuthMethod::Oidc))
            && !model.has_own_credentials()
            && credentials.auth_type == atelier_chat_state::AuthType::ApiKey
        {
            credentials.api_key = None;
            credentials.auth_type = atelier_chat_state::AuthType::SessionToken;
        }
        crate::agent::config::enforce_disable_api_key_auth(
            &mut credentials,
            self.cfg.borrow().atelier_com_config.api_key_auth_disabled(),
            session.as_ref().map(|a| a.key.as_str()),
        );
        if !has_session_key && credentials.auth_type == atelier_chat_state::AuthType::ApiKey
            && !model.has_own_credentials() && self.is_session_based_auth()
        {
            tracing::info!(
                model = model.info().model.as_str(),
                "auth: overriding auth_type to SessionToken (session-based auth method)",
            );
            atelier_telemetry::unified_log::info(
                "auth auth_type override to SessionToken",
                None,
                Some(serde_json::json!({ "model" : model.info().model.as_str() })),
            );
            credentials.auth_type = atelier_chat_state::AuthType::SessionToken;
        }
        if !has_session_key && !model.has_own_credentials() {
            tracing::warn!(
                model = model.info().model.as_str(), is_expired = self.auth_manager
                .is_expired(), auth_type = ? credentials.auth_type,
                "auth: prepare_sampling_config has no session key",
            );
            atelier_telemetry::unified_log::warn(
                "auth: prepare_sampling_config has no session key",
                None,
                Some(
                    serde_json::json!(
                        { "model" : model.info().model.as_str(), "is_expired" : self
                        .auth_manager.is_expired(), "auth_type" : format!("{:?}",
                        credentials.auth_type), }
                    ),
                ),
            );
        }
        let cfg = self.cfg.borrow();
        let alpha_test_key = cfg.endpoints.alpha_test_key.clone();
        let client_version = cfg.client_version.clone();
        let deployment_id = crate::managed_config::resolve_deployment_id(
            cfg.endpoints.deployment_key.as_deref(),
        );
        drop(cfg);
        let user_id = self
            .auth_manager
            .current_or_expired()
            .filter(|a| a.is_configured_refresh_auth())
            .map(|a| a.user_id);
        let mut config = crate::agent::config::sampling_config_for_model(
            model,
            credentials,
            alpha_test_key,
            client_version,
            deployment_id,
            user_id,
        );
        config.origin_client = origin_client;
        config
    }
    /// Resolve sampling config for a model by ID, falling back to the global
    /// default on resolution failure. This ensures API-key auth routes to
    /// the public API (via resolve_credentials) instead of the global config's
    /// cli-chat-proxy base_url.
    pub(super) fn resolve_sampling_config_for_model(
        &self,
        model_id: &acp::ModelId,
        origin_client: Option<crate::http::OriginClientInfo>,
    ) -> SamplingConfig {
        if let Ok(model) = self.resolve_model_id(model_id) {
            self.prepare_sampling_config_for_model(&model, origin_client.clone())
        } else {
            let mut c = self.sampling_config.borrow().clone();
            c.origin_client = origin_client;
            c
        }
    }
    /// Resolve `AgentDefinition.model` override for the parent session.
    /// Apply a profile's pinned-model override to the session's sampling config.
    ///
    /// `pinned_model` is resolved once by the caller (shared with harness
    /// inheritance). `None` — no override, or model not in catalog — keeps the
    /// session defaults.
    fn apply_agent_model_override(
        &self,
        pinned_model: Option<&(acp::ModelId, ModelEntry)>,
        default_model_id: acp::ModelId,
        default_sampling: SamplingConfig,
        origin_client: Option<crate::http::OriginClientInfo>,
    ) -> (acp::ModelId, SamplingConfig) {
        let Some((id, model)) = pinned_model else {
            return (default_model_id, default_sampling);
        };
        let new_config = self.prepare_sampling_config_for_model(model, origin_client);
        tracing::info!(
            model = % id.0, "agent profile model override applied to parent session"
        );
        (id.clone(), new_config)
    }
    /// Image generation is configured from the active exact Provider/model
    /// sampler route after the session actor has reconstructed dynamic auth.
    /// Keep agent construction disabled so rebuilds cannot restore a stale
    /// endpoint or the removed global xAI fallback.
    pub(super) fn prepare_image_gen_config(
        &self,
    ) -> atelier_tools::implementations::atelier_build::image_gen::ImageGenConfig {
        use atelier_tools::implementations::atelier_build::image_gen::ImageGenConfig;
        ImageGenConfig::Disabled
    }
    /// Build deploy-service config. The tool talks directly to the deployer service.
    pub(super) fn prepare_app_builder_deployer_config(
        &self,
    ) -> atelier_tools::implementations::atelier_build::deploy_app::AppBuilderDeployerConfig {
        use atelier_tools::implementations::atelier_build::deploy_app::AppBuilderDeployerConfig;
        AppBuilderDeployerConfig::Disabled
    }
    /// Video generation requires an exact Provider/model route. The removed
    /// process-global vendor endpoint must never be reconstructed here.
    pub(super) fn prepare_video_gen_config(
        &self,
    ) -> atelier_tools::implementations::atelier_build::video_gen::VideoGenConfig {
        use atelier_tools::implementations::atelier_build::video_gen::VideoGenConfig;
        VideoGenConfig::Disabled
    }
    pub(super) fn prepare_web_search_sampling_config(&self) -> Option<SamplingConfig> {
        let model_id = self.cfg.borrow().web_search_model.clone();
        let models = self.models_manager.models();
        let session = self.current_or_buffered_auth();
        let alpha_test_key = self.cfg.borrow().endpoints.alpha_test_key.clone();
        let client_version = self.cfg.borrow().client_version.clone();
        let mut cfg = config::resolve_web_search_sampling_config(
            &model_id,
            &models,
            session.as_ref().map(|a| a.key.as_str()),
            self.cfg.borrow().atelier_com_config.api_key_auth_disabled(),
            alpha_test_key.clone(),
            client_version,
            &self.cfg.borrow().endpoints,
        )?;
        inject_proxy_headers(
            &mut cfg.extra_headers,
            cfg.client_version.as_deref(),
            alpha_test_key.as_deref(),
            &cfg.base_url,
        );
        Some(cfg)
    }
    /// Returns `Err` with a user-facing message on invalid config; the caller at
    /// the process boundary prints it and exits.
    pub fn new(
        gateway: GatewaySender,
        cfg: &AgentConfig,
        auth_manager: Arc<AuthManager>,
        prefetched_models: Option<IndexMap<String, ModelEntry>>,
    ) -> Result<Self, String> {
        let (cfg, models_manager) = crate::agent::init::bootstrap(
            cfg,
            &auth_manager,
            prefetched_models,
        )?;
        Ok(Self::with_models(gateway, &cfg, auth_manager, models_manager))
    }
    /// Prepare the web fetch configuration based on feature flags.
    ///
    /// Enabled gate: `disable_web_search` kill-switch > `ATELIER_WEB_FETCH` env >
    /// remote settings `web_fetch_enabled` > default (false).
    ///
    /// Params resolution (TOML > env > remote settings > default):
    /// - `proxy_endpoint`: `[toolset.web_fetch] proxy_endpoint` > `ATELIER_WEB_FETCH_PROXY` > remote settings > None
    /// - `allowed_domains`: `[toolset.web_fetch] allowed_domains` > remote settings > built-in defaults
    pub(super) fn prepare_web_fetch_config(
        &self,
    ) -> atelier_tools::implementations::atelier_build::web_fetch::WebFetchConfig {
        use atelier_tools::implementations::atelier_build::web_fetch::WebFetchConfig;
        let cfg = self.cfg.borrow();
        if cfg.disable_web_search {
            return WebFetchConfig::Disabled;
        }
        let remote = cfg.local_runtime_settings.as_ref();
        let enabled = cfg.resolve_web_fetch();
        if !enabled.value {
            return WebFetchConfig::Disabled;
        }
        let context_window = Some(self.sampling_config.borrow().context_window);
        let params = cfg
            .toolset
            .web_fetch
            .resolve_params(
                remote.and_then(|s| s.web_fetch_proxy.as_deref()),
                remote.and_then(|s| s.web_fetch_allowed_domains.as_deref()),
                context_window,
            );
        if params.allowed_domains.as_ref().is_some_and(Vec::is_empty) {
            tracing::info!("web_fetch disabled: allowed_domains is explicitly empty");
            return WebFetchConfig::Disabled;
        }
        WebFetchConfig::Enabled { params }
    }
    /// Construct from pre-built components. Use when the caller needs the
    /// `ModelsManager` handle externally (e.g. `run_leader` wires it to the
    /// config watcher). Otherwise prefer [`Self::new`].
    pub fn with_models(
        gateway: GatewaySender,
        cfg: &AgentConfig,
        auth_manager: Arc<AuthManager>,
        models_manager: crate::agent::models::ModelsManager,
    ) -> Self {
        models_manager.set_gateway(gateway.clone());
        let sampling_config = models_manager.sampling_config();
        let storage_mode = cfg.storage_mode;
        let default_yolo_mode = cfg.default_yolo_mode;
        let default_auto_mode = cfg.default_auto_mode;
        let config_root = crate::config::load_effective_config().ok();
        let empty_config = toml::Value::Table(toml::map::Map::new());
        let raw = config_root.as_ref().unwrap_or(&empty_config);
        let (worktree_type, wt_source) = crate::util::config::resolve_worktree_type(
            raw,
            cfg.local_runtime_settings.as_ref(),
        );
        let restore_code = crate::util::config::resolve_restore_code(
            raw,
            cfg.local_runtime_settings.as_ref(),
        );
        let session_registry_local = config_root
            .as_ref()
            .and_then(crate::util::config::session_registry_from_toml_opt);
        tracing::info!(
            worktree_type = ? worktree_type, source = wt_source,
            "WORKTREE_CONFIG_SHELL: resolved worktree type at agent startup"
        );
        let (subagent_event_tx, subagent_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let activity = crate::agent::activity::AgentActivity::default();
        let mut subagent_coordinator = crate::agent::subagent::SubagentCoordinator::new();
        subagent_coordinator.set_running_gauge(activity.subagent_gauge());
        let instance = Self {
            sessions: RefCell::new(HashMap::new()),
            activity,
            loading_sessions: RefCell::new(HashMap::new()),
            prompt_intake_locks: RefCell::new(HashMap::new()),
            session_threads: RefCell::new(HashMap::new()),
            resident_roster_titles: RefCell::new(HashMap::new()),
            initialize_request: OnceLock::new(),
            gateway,
            subagent_model_overrides: cfg.subagent_model_overrides.clone(),
            subagent_toggle: cfg.subagent_toggle.clone(),
            launch_cwd: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
            launch_dir_trust: std::cell::OnceCell::new(),
            plugin_registry_handle: atelier_agent::plugins::SharedPluginRegistryHandle::new(
                None,
                cfg.plugins.cli_plugin_dirs.clone(),
            ),
            plugin_registry_initialized: std::cell::Cell::new(false),
            models_manager,
            cfg: RefCell::new(cfg.clone()),
            auth_method_id: crate::agent::auth_method::new_shared_auth_method_id(None),
            sampling_config: RefCell::new(sampling_config),
            auth_manager,
            auth_code_tx: RefCell::new(None),
            auth_url_rx: RefCell::new(None),
            client_type: RefCell::new(ClientType::default()),
            code_nav_enabled: std::cell::Cell::new(false),
            interactive_trust_client: std::cell::Cell::new(false),
            interactive_trust_prompted: Rc::new(
                RefCell::new(std::collections::HashSet::new()),
            ),
            tier_allowed: std::cell::Cell::new(true),
            storage_mode,
            default_yolo_mode,
            default_auto_mode,
            memory_config: None,
            config_watcher_path_tx: None,
            buffering_settings: RefCell::new(None),
            background_copy_context: BackgroundCopyContext::new(),
            session_turn_numbers: RefCell::new(HashMap::new()),
            permission_event_receivers: RefCell::new(HashMap::new()),
            codebase_indexes: Arc::new(
                parking_lot::Mutex::new(CodebaseIndexManager::new()),
            ),
            session_index_claims: RefCell::new(HashMap::new()),
            worktree_type,
            restore_code,
            session_registry_local,
            managed_mcp_cache: Default::default(),
            agent_mcp_state: std::sync::Arc::new(
                tokio::sync::Mutex::new(
                    crate::session::mcp_servers::McpState::new(vec![]),
                ),
            ),
            model_unavailable_sessions: RefCell::new(std::collections::HashMap::new()),
            subagent_event_tx,
            subagent_event_rx: RefCell::new(Some(subagent_event_rx)),
            subagent_coordinator: RefCell::new(subagent_coordinator),
            monitor_event_buffer: atelier_tools::implementations::atelier_build::task::types::MonitorEventBuffer::default(),
            post_unblock_jwt_retry_in_flight: Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            workspace_ops: RefCell::new(None),
            session_workspace_ops: RefCell::new(HashMap::new()),
            require_gateway_sessions: Rc::new(
                RefCell::new(std::collections::HashSet::new()),
            ),
            session_live_state: RefCell::new(HashMap::new()),
            runtime_control: Arc::new(parking_lot::Mutex::new(
                crate::runtime_control::RuntimeControl::default(),
            )),
            policy_engine: Arc::new(parking_lot::RwLock::new(
                atelier_hooks::PolicyEngine::default(),
            )),
            retryable_prompts: RefCell::new(HashMap::new()),
            detached_prompt_waiters: RefCell::new(HashMap::new()),
            runtime_subscriptions: RefCell::new(HashSet::new()),
            supervisor_started: std::cell::Cell::new(false),
            announcements_gen: std::cell::Cell::new(0),
            last_emitted_announcements: RefCell::new(Vec::new()),
            announcements_refresh_started: std::cell::Cell::new(false),
            heap_profile_monitor: RefCell::new(
                crate::heap_profile::HeapProfileMonitor::new(),
            ),
            heap_profile_started: std::cell::Cell::new(false),
            #[cfg(test)]
            finalize_spy: RefCell::new(Vec::new()),
            #[cfg(test)]
            roster_delta_spy: RefCell::new(Vec::new()),
            #[cfg(test)]
            supervisor_spawn_count: std::cell::Cell::new(0),
        };
        instance
            .auth_manager
            .configure_refresher(instance.cfg.borrow().atelier_com_config.auth_provider_command.clone());
        instance
    }
    /// Handle `atelier/internal/evict_sessions` — the leader server tells us a
    /// client disconnected and these sessions lost their IPC owner.
    ///
    /// **This is the no-evict keystone.** A disconnect must
    /// NOT destroy a session. The behavior is now *detach + keep-resident +
    /// idle-unload*:
    ///
    /// - **Sessions with live work stay resident.** We do NOT send `Shutdown`
    ///   and do NOT drop the `SessionHandle`, so the actor, its pending
    ///   permission oneshots, and its `KillOnDrop` tool subprocesses all
    ///   survive. The route/driver detach is groundwork for PR-3 (the
    ///   driver/subscriber maps don't exist yet), so for now we only mark the
    ///   live state.
    /// - **Fully idle sessions are unloaded to disk** to bound memory (the
    ///   `sessions`/`session_threads` maps are uncapped). This preserves the
    ///   legacy unload path — `Shutdown` the actor, drop the `SessionHandle`,
    ///   but KEEP the `SessionThread` so `drain_old_session_thread` can drain it
    ///   on reconnect — and crucially does **not** finalize the cloud replica
    ///   (the session remains resumable via `session/load`).
    ///
    /// The "live work" check is the coarse PR-2 stub (`session_has_live_work`);
    /// the full `SessionActivity` signal lands in PR-4.
    pub(super) async fn handle_evict_sessions(
        &self,
        params: &serde_json::value::RawValue,
    ) {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct EvictParams {
            session_ids: Vec<String>,
        }
        let Ok(p) = serde_json::from_str::<EvictParams>(params.get()) else {
            tracing::warn!("Failed to parse evict_sessions params");
            return;
        };
        if p.session_ids.is_empty() {
            return;
        }
        tracing::info!(
            count = p.session_ids.len(), sessions = ? p.session_ids,
            "Client disconnected; detaching sessions (no-evict keystone)"
        );
        let checks = p
            .session_ids
            .iter()
            .map(|sid| {
                let id = acp::SessionId::new(sid.clone());
                async move {
                    let busy = self.session_has_live_work(&id).await;
                    (id, busy)
                }
            });
        let resolved = futures::future::join_all(checks).await;
        let mut kept_resident: usize = 0;
        let mut unloaded: usize = 0;
        for (id, busy) in resolved {
            if busy {
                self.set_session_live_state(&id, SessionLiveState::Working);
                kept_resident += 1;
                tracing::info!(
                    session_id = % id.0,
                    "kept session resident across client disconnect (live work)"
                );
                continue;
            }
            self.request_session_shutdown(&id);
            if let Some(handle) = self.sessions.borrow_mut().remove(&id) {
                handle.workspace_ops.end_local_session(&id.0);
                self.session_index_claims.borrow_mut().remove(&id);
                self.require_gateway_sessions.borrow_mut().remove(&id);
                self.set_session_live_state(&id, SessionLiveState::Dormant);
                unloaded += 1;
                tracing::debug!(
                    session_id = % id.0, "idle session unloaded to disk on disconnect"
                );
            }
        }
        tracing::info!(kept_resident, unloaded, "client-disconnect detach complete");
        self.sweep_dead_sessions();
    }
    /// Wait for an old session thread to finish before reloading the same session.
    ///
    /// When a client disconnects and a session is *idle*, `handle_evict_sessions`
    /// unloads it: sends `Shutdown`, drops the `SessionHandle`, and keeps the
    /// `SessionThread`. (Sessions with live work stay fully resident and skip
    /// this path.) If the client reconnects and loads the same session, we must
    /// wait for the old actor to finish flushing to disk before replaying
    /// `updates.jsonl`.
    ///
    /// Uses async polling (never blocks the `LocalSet` runtime) with a 5s deadline
    /// to handle slow shutdowns (e.g., embedding API timeouts).
    pub(super) async fn drain_old_session_thread(&self, session_id: &acp::SessionId) {
        let thread = self.session_threads.borrow_mut().remove(session_id);
        let Some(thread) = thread else { return };
        if thread.is_finished() {
            return;
        }
        tracing::info!(
            session_id = % session_id.0,
            "Waiting for old session thread to finish before reload"
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if thread.is_finished() {
                tracing::debug!(
                    session_id = % session_id.0, "Old session thread finished cleanly"
                );
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    session_id = % session_id.0,
                    "Old session thread still running after 5s — proceeding with replay. \
                     Session data may be incomplete if the old actor is still writing."
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
    /// Mark a `session/load` as in flight for `session_id`.
    ///
    /// Returns an RAII guard; while it is alive,
    /// [`Self::wait_for_in_flight_session_load`] blocks racing session-scoped
    /// requests for the same session. Dropping the guard (every exit path of
    /// `load_session`, success or error) removes the marker and wakes all
    /// waiters via watch-channel closure.
    pub(super) fn begin_session_load(
        &self,
        session_id: &acp::SessionId,
    ) -> SessionLoadGuard<'_> {
        let (tx, rx) = tokio::sync::watch::channel(false);
        self.loading_sessions.borrow_mut().insert(session_id.clone(), rx.clone());
        SessionLoadGuard {
            agent: self,
            session_id: session_id.clone(),
            rx,
            _tx: tx,
        }
    }
    /// Session lookup that tolerates an in-flight `session/load`.
    ///
    /// THE chokepoint for the post-leader-crash error class: every
    /// user-facing session-scoped handler (`prompt`, `set_session_model`,
    /// `set_session_mode`, `interject`, ...) resolves its handle through
    /// this instead of a bare `sessions` lookup, so a request racing the
    /// reconnect-replayed `session/load` waits for the session to land
    /// rather than failing with "unknown session id" / "session not found".
    ///
    /// Returns `None` only when the session is genuinely absent — no load in
    /// flight (or the load failed / timed out), exactly the cases where the
    /// legacy error is correct.
    pub(crate) async fn session_handle_waiting_for_load(
        &self,
        session_id: &acp::SessionId,
    ) -> Option<crate::session::SessionHandle> {
        let existing = self.sessions.borrow().get(session_id).cloned();
        if existing.is_some() {
            return existing;
        }
        self.wait_for_in_flight_session_load(session_id).await;
        self.sessions.borrow().get(session_id).cloned()
    }
    /// If a `session/load` for `session_id` is in flight, wait (bounded) for
    /// it to finish. Returns immediately when no load is in flight.
    ///
    /// This closes the load-vs-request race after a leader restart: clients
    /// replay `session/load` on reconnect, and a `session/prompt` arriving
    /// right behind it must wait for the session to land in `self.sessions`
    /// instead of failing with "unknown session id". The wait wakes when the
    /// load's [`SessionLoadGuard`] drops (success or failure) and re-checks;
    /// a failed load still surfaces the original error to the caller.
    pub(crate) async fn wait_for_in_flight_session_load(
        &self,
        session_id: &acp::SessionId,
    ) {
        const LOAD_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(
            60,
        );
        let deadline = tokio::time::Instant::now() + LOAD_WAIT_TIMEOUT;
        loop {
            if self.sessions.borrow().contains_key(session_id) {
                return;
            }
            let rx = self.loading_sessions.borrow().get(session_id).cloned();
            let Some(mut rx) = rx else { return };
            let now = tokio::time::Instant::now();
            if now >= deadline {
                tracing::warn!(
                    session_id = % session_id.0,
                    "timed out waiting for in-flight session/load"
                );
                return;
            }
            let _ = tokio::time::timeout(deadline - now, rx.changed()).await;
        }
    }
    /// Returns the default YOLO mode setting for new sessions
    pub fn default_yolo_mode(&self) -> bool {
        self.default_yolo_mode
    }
    /// Returns the storage mode configured for this agent
    pub fn storage_mode(&self) -> StorageMode {
        self.storage_mode
    }
    /// Returns the background copy context for managing background file copy tasks.
    pub fn background_copy_context(&self) -> BackgroundCopyContext {
        self.background_copy_context.clone()
    }
    /// Move a foreground bash command to background.
    /// Routes through the session's tool bridge to unblock the agent loop.
    pub async fn background_foreground_command(
        &self,
        session_id: &str,
        tool_call_id: &str,
    ) -> bool {
        let sid = acp::SessionId::new(session_id);
        if let Some(handle) = self.get_session_handle(&sid) {
            handle.background_foreground_command(tool_call_id).await
        } else {
            false
        }
    }
    /// Kill a background task by task_id.
    /// Routes through the session's tool bridge to the TerminalBackend.
    pub async fn kill_background_task(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<atelier_tools::types::KillOutcome, String> {
        let sid = acp::SessionId::new(session_id);
        if let Some(handle) = self.get_session_handle(&sid) {
            handle.kill_background_task(task_id).await
        } else {
            Err("session not found".to_string())
        }
    }
    pub async fn delete_scheduled_task(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<bool, String> {
        let sid = acp::SessionId::new(session_id);
        if let Some(handle) = self.get_session_handle(&sid) {
            handle.delete_scheduled_task(task_id).await
        } else {
            Err("session not found".to_string())
        }
    }
    /// Cancel a subagent by id, returning a typed outcome that backs the pager's
    /// `atelier/subagent/cancel`. Active/pending → cancelled (a finish follows);
    /// already-finished → its terminal status; unknown id → `NotFound`.
    pub fn cancel_subagent(
        &self,
        subagent_id: &str,
    ) -> atelier_tools::implementations::atelier_build::task::types::SubagentCancelOutcome {
        self.subagent_coordinator.borrow_mut().cancel_with_outcome(subagent_id)
    }
    /// List running subagent seeds for a given parent session.
    ///
    /// Synchronously collects seeds from the coordinator, suitable for
    /// async resolution via `resolve_running_list()` after the borrow is
    /// dropped.
    pub(crate) fn list_running_subagents(
        &self,
        parent_session_id: &str,
    ) -> Vec<crate::agent::subagent::RunningSubagentListSeed> {
        self.subagent_coordinator.borrow().list_running_for_parent(parent_session_id)
    }
    /// Return fork provenance metadata for a subagent.
    pub(crate) fn provenance_for_subagent(
        &self,
        subagent_id: &str,
    ) -> crate::agent::subagent::SubagentProvenance {
        self.subagent_coordinator.borrow().provenance_for(subagent_id)
    }
    /// Return `(parent_session_id, child_session_id)` for a subagent.
    pub(crate) fn session_ids_for_subagent(
        &self,
        subagent_id: &str,
    ) -> Option<(String, String)> {
        self.subagent_coordinator.borrow().session_ids_for(subagent_id)
    }
    /// Synchronous lookup of a single subagent by ID.
    ///
    /// Returns `Option<SnapshotLookup>` which must be resolved
    /// asynchronously via `resolve_snapshot()` after the borrow is dropped.
    pub(crate) fn lookup_subagent(
        &self,
        subagent_id: &str,
    ) -> Option<crate::agent::subagent::SnapshotLookup> {
        self.subagent_coordinator.borrow().lookup(subagent_id)
    }
    /// List all background tasks for a session.
    /// Routes through the session's tool bridge to the TerminalBackend.
    pub async fn list_tasks(
        &self,
        session_id: &str,
    ) -> Option<Vec<atelier_tools::types::TaskSnapshot>> {
        let sid = acp::SessionId::new(session_id);
        if let Some(handle) = self.get_session_handle(&sid) {
            handle.list_tasks().await
        } else {
            None
        }
    }
    /// Flush a session's persistence buffer with a 5-second timeout.
    ///
    /// Sends `FlushComplete` to the session actor, which chains through to
    /// `FlushAndAck` on the persistence actor — a true sync barrier that only
    /// resolves after all queued writes (chat messages, updates) hit disk.
    ///
    /// Returns `Ok(())` on success, `Err(reason)` on timeout or channel failure.
    pub(crate) async fn flush_session(
        &self,
        session_id: &acp::SessionId,
    ) -> Result<(), &'static str> {
        let cmd_tx = self.sessions.borrow().get(session_id).map(|h| h.cmd_tx.clone());
        let Some(cmd_tx) = cmd_tx else {
            return Err("session not found");
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        if cmd_tx
            .send(SessionCommand::FlushComplete {
                respond_to: tx,
            })
            .is_err()
        {
            return Err("send failed");
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err("channel closed"),
            Err(_) => Err("timeout"),
        }
    }
    /// Get a session's cwd by session_id.
    /// Returns None if the session is not found.
    pub fn get_session_cwd(&self, session_id: &acp::SessionId) -> Option<PathBuf> {
        let sessions = self.sessions.borrow();
        sessions.get(session_id).map(|handle| PathBuf::from(&handle.info.cwd))
    }
    /// Get a session handle by session_id.
    /// Returns None if the session is not found.
    pub fn get_session_handle(
        &self,
        session_id: &acp::SessionId,
    ) -> Option<crate::session::SessionHandle> {
        let sessions = self.sessions.borrow();
        sessions.get(session_id).cloned()
    }
    /// Get hooks list for a session (for `atelier/hooks/list` extension).
    pub async fn list_hooks(
        &self,
        session_id: &acp::SessionId,
    ) -> Option<atelier_hooks_plugins_types::HooksListResponse> {
        let handle = self.get_session_handle(session_id)?;
        handle.get_hooks_list().await
    }
    /// Execute a hooks management action (for `atelier/hooks/action`).
    pub async fn execute_hooks_action(
        &self,
        session_id: &acp::SessionId,
        action: atelier_hooks_plugins_types::HooksAction,
    ) -> Option<atelier_hooks_plugins_types::ActionOutcome> {
        if matches!(action, atelier_hooks_plugins_types::HooksAction::Untrust)
            && let Some(cwd) = self.get_session_cwd(session_id)
        {
            self.interactive_trust_prompted
                .borrow_mut()
                .remove(&atelier_workspace::trust::workspace_key(&cwd));
        }
        let handle = self.get_session_handle(session_id)?;
        handle.execute_hooks_action(action).await
    }
    /// Execute a plugins management action (for `atelier/plugins/action`).
    pub async fn execute_plugins_action(
        &self,
        session_id: &acp::SessionId,
        action: atelier_hooks_plugins_types::PluginsAction,
    ) -> Option<atelier_hooks_plugins_types::ActionOutcome> {
        let is_reload = matches!(action, atelier_hooks_plugins_types::PluginsAction::Reload);
        let handle = self.get_session_handle(session_id)?;
        let outcome = handle.execute_plugins_action(action).await;
        let succeeded = matches!(
            outcome.as_ref().map(| o | & o.status),
            Some(atelier_hooks_plugins_types::OutcomeStatus::Success)
        );
        if is_reload && succeeded {
            self.broadcast_plugin_registry_to_sessions(Some(session_id));
        }
        outcome
    }
    /// Get a snapshot of the shared plugin registry (for `atelier/plugins/list`).
    pub fn plugin_registry_snapshot(
        &self,
    ) -> Option<std::sync::Arc<atelier_agent::plugins::PluginRegistry>> {
        self.plugin_registry_handle.snapshot()
    }
    /// Resolve client version: prefer the value from the initialize request _meta,
    /// fall back to the agent's own version (VERSION_WITH_COMMIT set by the TUI launcher).
    pub(super) fn client_version(&self) -> Option<String> {
        self.initialize_request
            .get()
            .and_then(|req| req.meta.as_ref())
            .and_then(|m| m.get("clientVersion"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| self.cfg.borrow().client_version.clone())
    }
    pub(super) fn origin_client_info_from_meta(
        &self,
        meta: Option<&acp::Meta>,
    ) -> Option<crate::http::OriginClientInfo> {
        crate::http::merge_origin_client_info(
                crate::http::origin_client_info_from_meta(meta),
                crate::http::origin_client_info_from_meta(
                        self.initialize_request.get().and_then(|req| req.meta.as_ref()),
                    )
                    .map(|mut origin| {
                        if origin.version.is_none() {
                            origin.version = self.client_version();
                        }
                        origin
                    }),
            )
            .map(|mut origin| {
                if origin.version.is_none() {
                    origin.version = self.client_version();
                }
                origin
            })
    }
    /// Returns the model state for a given session (or the agent default).
    ///
    /// When `session_id` is `Some`, looks up the session's per-session model.
    /// Falls back to `current_model_id` (startup default) when no session is
    /// found or `session_id` is `None` (e.g., during `initialize` before any
    /// session exists).
    pub fn model_state(
        &self,
        session_id: Option<&acp::SessionId>,
    ) -> acp::SessionModelState {
        let model_id = lookup_session_model(
            &self.sessions.borrow(),
            session_id,
            &self.models_manager.current_model_id(),
        );
        let mut available_models: Vec<acp::ModelInfo> = self
            .models_manager
            .available()
            .values()
            .cloned()
            .collect();
        let override_effort = session_id
            .and_then(|sid| self.sessions.borrow().get(sid).map(|h| h.reasoning_effort))
            .flatten()
            .or_else(|| self.models_manager.current_reasoning_effort());
        if let Some(override_effort) = override_effort
            && let Some(info) = available_models
                .iter_mut()
                .find(|info| info.model_id == model_id)
            && supports_reasoning_effort_meta(info.meta.as_ref())
        {
            let mut map = info.meta.clone().unwrap_or_default();
            map.insert(
                REASONING_EFFORT_META_KEY.to_string(),
                reasoning_effort_meta_value(override_effort),
            );
            info.meta = Some(map);
        }
        acp::SessionModelState::new(model_id, available_models)
    }
    pub(super) fn session_config_options(
        &self,
        session_id: Option<&acp::SessionId>,
        state: &acp::SessionModelState,
    ) -> Vec<session_config::SessionConfigOption> {
        let model_id = resolve_catalog_key(
                &self.models_manager.models(),
                &state.current_model_id,
            )
            .unwrap_or_else(|| state.current_model_id.clone());
        let supports_effort = self
            .models_manager
            .model_supports_reasoning_effort(model_id.0.as_ref());
        let effort_options: Vec<ReasoningEffortOption> = if supports_effort {
            let options = self
                .models_manager
                .model_reasoning_efforts(model_id.0.as_ref());
            if options.is_empty() {
                session_config::legacy_session_effort_options()
            } else {
                options
            }
        } else {
            Vec::new()
        };
        let current_effort = if supports_effort {
            session_id
                .and_then(|sid| {
                    self.sessions.borrow().get(sid).map(|h| h.reasoning_effort)
                })
                .flatten()
                .or_else(|| self.models_manager.current_reasoning_effort())
                .or_else(|| {
                    self
                        .models_manager
                        .model_default_reasoning_effort(model_id.0.as_ref())
                })
        } else {
            None
        };
        session_config::build_session_config_options(
            &state.available_models,
            &model_id,
            &effort_options,
            current_effort,
        )
    }
    /// Build the `atelier/sessionConfig` and `atelier/sessionDetail` `_meta` values
    /// shared by `new_session` and `load_session`, returned as
    /// `(sessionConfig, sessionDetail)`. Keeping both response paths on this one
    /// builder stops them drifting.
    pub(super) fn session_config_meta(
        &self,
        session_id: &acp::SessionId,
        cwd: String,
        title: Option<String>,
        model_state: &acp::SessionModelState,
    ) -> (serde_json::Value, serde_json::Value) {
        let config_options = self.session_config_options(Some(session_id), model_state);
        let detail = session_config::AtelierSessionDetail::build(
            session_id.0.to_string(),
            cwd,
            model_state.current_model_id.0.to_string(),
            title,
        );
        (serde_json::json!({ "options" : config_options }), serde_json::json!(detail))
    }
    /// Seed the global sampling config with login auth when available.
    ///
    /// Only sets the `api_key` if missing. Does NOT resolve `base_url` from
    /// `current_model_id` — that's deferred to session creation time to avoid
    /// cross-client contamination in leader mode (where `current_model_id` is
    /// shared mutable state).
    pub(super) fn seed_client_config_auth_if_available(&self) {
        let mut sampling_config = self.sampling_config.borrow_mut();
        if sampling_config.api_key.is_none() {
            if let Some(auth) = self.auth_manager.current_or_expired() {
                sampling_config.api_key = Some(auth.key);
                tracing::debug!("auth: seed_client_config set auth (SessionToken)");
                atelier_telemetry::unified_log::debug(
                    "auth: seed_client_config set auth (SessionToken)",
                    None,
                    None,
                );
            } else if !self
                .models_manager
                .models()
                .values()
                .any(|m| m.has_own_credentials())
            {
                tracing::warn!(
                    "No credentials found: no login token and no model api_key/env_key"
                );
                atelier_telemetry::unified_log::warn(
                    "No credentials found: no login token and no model api_key/env_key",
                    None,
                    None,
                );
            }
        }
    }
    /// Allocate the next monotonic local-artifact turn number for a session.
    ///
    /// Returns the current turn number and advances the counter. The counter is
    /// intentionally monotonic even across rewinds to avoid overwriting older
    /// local diagnostic artifacts.
    ///
    /// For sessions sharing a parent's trace counter, call this once with the
    /// **root session ID** and reuse the result so the root's counter does not
    /// advance more than once per logical turn. The local artifact layout writes to
    /// `{session_id}/turn_{N}/`.
    pub(crate) fn allocate_turn_number(&self, session_id: &acp::SessionId) -> u64 {
        let turn = self.peek_turn_number(session_id);
        self.set_turn_number(session_id, turn.saturating_add(1));
        turn
    }
    /// Read a session's next trace turn number without advancing the counter.
    fn peek_turn_number(&self, session_id: &acp::SessionId) -> u64 {
        self.session_turn_numbers.borrow().get(session_id).copied().unwrap_or(0u64)
    }
    /// Set a session's next trace turn number. The sole writer of the
    /// `session_turn_numbers` counter, shared by `allocate_turn_number` and the
    /// batched harness-sibling allocation so both honor the same storage.
    fn set_turn_number(&self, session_id: &acp::SessionId, next: u64) {
        self.session_turn_numbers.borrow_mut().insert(session_id.clone(), next);
    }
    /// Persist each drained harness trace turn (the goal planner at setup, and
    /// each verifier skeptic panel) as its OWN sibling `turn_{N}` artifact.
    ///
    /// These phases run inside the single user-facing goal turn but are
    /// recorded out-of-band (synthetic `task` pairs in a side buffer), so the
    /// normal per-round `turn_messages.json` never references them. Giving each
    /// phase its own monotonic turn number — from the SAME `session_turn_numbers`
    /// counter the model turns use (see [`Self::allocate_turn_number`]), via
    /// [`Self::get_trace_context`] plus the local artifact writers keeps these
    /// synthetic turns available for local diagnostics.
    /// The advanced counter is persisted via `SetNextTraceTurn` so the siblings
    /// survive a restart. Best-effort and non-blocking.
    pub(super) async fn persist_harness_trace_turns(
        &self,
        session_id: &acp::SessionId,
        info: &crate::session::info::Info,
        cmd_tx: &tokio::sync::mpsc::UnboundedSender<crate::session::SessionCommand>,
        model: &str,
        turns: Vec<Vec<atelier_sampling_types::conversation::ConversationItem>>,
    ) {
        let base = self.peek_turn_number(session_id);
        let artifacts = self
            .prepare_harness_trace_artifacts(session_id, info, model, base, turns)
            .await;
        if artifacts.is_empty() {
            return;
        }
        let next_trace_turn = base.saturating_add(artifacts.len() as u64);
        self.set_turn_number(session_id, next_trace_turn);
        let _ = cmd_tx
            .send(crate::session::SessionCommand::SetNextTraceTurn {
                next_trace_turn,
                request_id: None,
            });
        for (ctx, metadata, capture) in artifacts {
            spawn_artifact_task(
                "harness_trace_turn",
                async move {
                    let session_state = build_chat_history_session_state(
                        &capture.messages,
                    );
                    futures::join!(
                        write_metadata(&ctx, metadata), write_turn_messages(&ctx,
                        capture), write_harness_session_archive(&
                        ctx, session_state),
                    );
                    crate::local_artifacts::artifacts::write_manifest(&ctx).await;
                },
            );
        }
    }
    /// Number the drained harness turns `base, base+1, …` and build their
    /// `(trace context, metadata, capture)` local artifact records. Stops at the
    /// first turn whose context is `None`, which means the session is gone. A
    /// `None` after a `Some` would be a broken invariant, so it is logged rather
    /// than dropped silently.
    pub(super) async fn prepare_harness_trace_artifacts(
        &self,
        session_id: &acp::SessionId,
        info: &crate::session::info::Info,
        model: &str,
        base: u64,
        turns: Vec<Vec<atelier_sampling_types::conversation::ConversationItem>>,
    ) -> Vec<(PromptTraceContext, PromptMetadata, atelier_chat_state::TurnCapture)> {
        let mut artifacts = Vec::with_capacity(turns.len());
        for (offset, items) in turns.into_iter().enumerate() {
            let turn_number = base.saturating_add(offset as u64);
            let Some(ctx) = self.get_trace_context(info, turn_number).await else {
                if offset > 0 {
                    tracing::warn!(
                        turn_number,
                        "harness trace: trace context unexpectedly None mid-batch; \
                         dropping the remaining drained turns"
                    );
                }
                break;
            };
            let metadata = PromptMetadata {
                schema_version: LOCAL_ARTIFACT_SCHEMA_VERSION.to_string(),
                session_id: session_id.0.to_string(),
                turn_number,
                request_id: format!("harness-trace-{turn_number}"),
                turn_started_at: chrono::Utc::now().to_rfc3339(),
                repo_root: None,
                remote_url: None,
                user_id: None,
                user_email: None,
                team_id: None,
                client_source: None,
                client_version: None,
                model: model.to_string(),
                reasoning_effort: ctx
                    .session_handle
                    .reasoning_effort
                    .map(|e| e.as_str().to_string()),
                experiment_id: None,
                host_os: std::env::consts::OS.to_string(),
                host_arch: std::env::consts::ARCH.to_string(),
                prompt_has_image: Some(false),
                prompt_was_truncated: Some(false),
                prompt_verbatim: Some(true),
                cwd: Some(info.cwd.clone()),
                agent_type: None,
                shell_version: Some(atelier_version::VERSION.to_string()),
                workspace_type: None,
                sandbox: local_sandbox_telemetry(),
            };
            let capture = atelier_chat_state::TurnCapture {
                messages: items,
                compaction_occurred: false,
            };
            artifacts.push((ctx, metadata, capture));
        }
        artifacts
    }
    /// Gets the local artifact context for a prompt.
    pub(crate) async fn get_trace_context(
        &self,
        session_info: &crate::session::info::Info,
        turn_number: u64,
    ) -> Option<PromptTraceContext> {
        let session_handle = match self.sessions.borrow().get(&session_info.id) {
            Some(h) => h.clone(),
            None => {
                tracing::Span::current().record("local_artifacts_enabled", false);
                tracing::Span::current()
                    .record("local_artifact_reason", "session_not_found");
                return None;
            }
        };
        let session_registry_enabled = false;
        Some(PromptTraceContext {
            session_info: session_info.clone(),
            turn_number,
            session_handle,
            session_registry_enabled,
            local_artifact_state: crate::local_artifacts::manifest::new_local_artifact_state(),
        })
    }
    /// Resolve the agent definition for a session.
    ///
    /// Priority (highest to lowest):
    /// 1. Model `agent_type` if it names a strict harness (codex, …).
    /// 2. Built-in `agent_config` name from config.toml `[agent]`.
    /// 3. Built-in `ATELIER_AGENT` name.
    /// 4. Built-in default agent.
    ///
    /// `ATELIER_AGENT` and an explicit `[agent] name` bypass step 1.
    /// Strict-harness classification is structural — see
    /// [`atelier_agent::config::is_strict_harness_agent_type`].
    ///
    /// Harness inheritance for a profile that pins its own model is applied by
    /// the caller via [`inherited_harness_template`], not here.
    pub fn resolve_agent_definition(
        cwd: &std::path::Path,
        agent_config: &config::AgentSelectionConfig,
        model_agent_type: Option<&str>,
    ) -> atelier_agent::AgentDefinition {
        use atelier_agent::AgentDefinition;
        let atelier_agent_env_set = std::env::var("ATELIER_AGENT")
            .ok()
            .is_some_and(|s| !s.trim().is_empty());
        let config_agent_explicitly_set = agent_config.name.is_some();
        let model_requires_strict_harness = model_agent_type
            .is_some_and(atelier_agent::config::is_strict_harness_agent_type);
        if !atelier_agent_env_set && !config_agent_explicitly_set
            && model_requires_strict_harness && let Some(required) = model_agent_type
            && let Some(def) = atelier_agent::discovery::by_name_in_cwd(required, cwd)
        {
            tracing::info!(
                agent_name = % def.name, "Using agent definition from model agent_type"
            );
            return def;
        }
        if let Some(ref name) = agent_config.name {
            tracing::info!(
                agent_name = % name,
                "Resolving built-in agent definition from config.toml [agent] name"
            );
            return atelier_agent::discovery::by_name(name).unwrap_or_else(|| {
                panic!("unsupported [agent].name `{name}`; only built-in Agent harnesses are allowed")
            });
        }
        let agent_name = std::env::var("ATELIER_AGENT")
            .ok()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty());
        let resolved = match agent_name {
            Some(name) => atelier_agent::discovery::by_name(&name).unwrap_or_else(|| {
                panic!(
                    "unsupported ATELIER_AGENT value `{name}`; only built-in Agent harnesses are allowed"
                )
            }),
            None => AgentDefinition::atelier_build_plan(),
        };
        if !atelier_agent_env_set && !config_agent_explicitly_set
            && model_requires_strict_harness && let Some(required) = model_agent_type
            && resolved.name != required
        {
            tracing::info!(
                resolved_agent = % resolved.name, model_agent_type = % required,
                "resolve_agent_definition: model requires different agent, re-resolving"
            );
            if let Some(def) = atelier_agent::discovery::by_name_in_cwd(required, cwd) {
                return def;
            }
            tracing::warn!(
                model_agent_type = % required, fallback_agent = % resolved.name,
                "resolve_agent_definition: model agent_type '{}' not found via discovery, \
                 keeping chain-resolved agent",
                required,
            );
        }
        resolved
    }
    /// Extract per-client terminal/fs capabilities from request `_meta`
    /// (injected by the leader). Falls back to the shared `init` OnceCell.
    pub(super) fn resolve_client_io_caps(
        meta: Option<&acp::Meta>,
        init: &acp::InitializeRequest,
    ) -> (bool, bool, bool) {
        let terminal = meta
            .and_then(|m| m.get("clientTerminal"))
            .and_then(|v| v.as_bool())
            .unwrap_or(init.client_capabilities.terminal);
        let fs_read = meta
            .and_then(|m| m.get("clientFsRead"))
            .and_then(|v| v.as_bool())
            .unwrap_or(init.client_capabilities.fs.read_text_file);
        let fs_write = meta
            .and_then(|m| m.get("clientFsWrite"))
            .and_then(|v| v.as_bool())
            .unwrap_or(init.client_capabilities.fs.write_text_file);
        (terminal, fs_read, fs_write)
    }
    /// Spawn and register a session actor given a session id and session parameters.
    ///
    /// Parameters are bundled in [`SessionSpawnOptions`] (named fields) rather than
    /// passed positionally: there are too many same-typed args (`bool`s,
    /// `Option<…>`s) for positional calls to be transposition-safe.
    pub(super) async fn spawn_and_register_session(
        &self,
        init: &acp::InitializeRequest,
        spec: SessionSpawnOptions<'_>,
    ) -> Result<(), acp::Error> {
        let SessionSpawnOptions {
            session_info,
            cwd,
            mcp_servers,
            initial_client_mcp_servers,
            mcp_meta_config_map,
            persistence,
            mut chat_history,
            rewind_points_file_path,
            initial_total_tokens,
            origin_client: _origin_client,
            client_code_nav_enabled,
            client_terminal,
            client_fs_read,
            client_fs_write,
            preloaded_envrc,
            persisted_signals,
            persisted_plan_mode,
            persisted_goal_mode,
            persisted_announcement_state,
            session_meta,
            managed_mcp_expires_at,
            model_agent_type,
            mut session_model_id,
            session_role,
            session_yolo_mode,
            session_auto_mode,
            prompt_display_cwd,
        } = spec;
        let _timer = crate::instrumentation_timer!("session.spawn_and_register");
        reject_direct_hub_cloud_meta(session_meta)?;
        let spawn_local_runtime_settings = self.cfg.borrow().local_runtime_settings.clone();
        folder_trust::resolve_and_record(
            cwd.as_path(),
            spawn_local_runtime_settings.as_ref(),
            false,
        );
        let use_acp_fs = client_fs_read && client_fs_write;
        let fs_notify_config = init
            .client_capabilities
            .meta
            .as_ref()
            .and_then(|m| m.get("atelier/fs_notify"))
            .and_then(|v| {
                use crate::session::{ClientFsConfig, ClientFsMode};
                use atelier_fsnotify::FsConfig;
                if v.as_bool() == Some(true) {
                    return Some(ClientFsConfig::default());
                }
                let obj = v.as_object()?;
                if obj.get("enabled").and_then(|e| e.as_bool()) == Some(false) {
                    return None;
                }
                let mode = if obj.get("index").and_then(|i| i.as_bool()) == Some(true) {
                    ClientFsMode::Index
                } else {
                    ClientFsMode::Events
                };
                let mut fs = FsConfig::default();
                if let Some(ms) = obj.get("debounce_ms").and_then(|v| v.as_u64()) {
                    fs.debounce_ms = ms;
                }
                if let Some(patterns) = obj.get("ignore").and_then(|v| v.as_array()) {
                    fs.ignore_patterns = patterns
                        .iter()
                        .filter_map(|p| p.as_str().map(String::from))
                        .collect();
                }
                Some(ClientFsConfig { fs, mode })
            });
        let workspace_ops = self
            .resolve_session_workspace_ops(cwd.as_path())
            .map_err(|_| {
                acp::Error::internal_error()
                    .data(
                        "Local workspace initialization failed; cannot create session. \
                 Check that a Tokio runtime is available.",
                    )
            })?;
        let filesystem_mode = session_filesystem_mode(use_acp_fs, cfg!(test));
        let fs: Arc<dyn atelier_workspace::file_system::AsyncFileSystem> =
            match filesystem_mode {
                SessionFilesystemMode::AcpClient => {
                    let mut acp_fs = AcpSessionFs::new(
                        cwd.to_path_buf(),
                        session_info.id.clone(),
                        self.gateway.clone(),
                    );
                    if let Some(ref display) = prompt_display_cwd {
                        acp_fs = acp_fs.with_display_cwd(std::path::PathBuf::from(display));
                    }
                    Arc::new(acp_fs)
                }
                SessionFilesystemMode::InProcessTest => {
                    Arc::new(atelier_workspace::file_system::LocalFs::new(cwd.to_path_buf()))
                }
                SessionFilesystemMode::WorkspaceWorker => {
                    let worker_path =
                        atelier_workspace::WorkspaceWorkerClient::default_worker_path()
                            .map_err(|error| {
                                acp::Error::internal_error().data(format!(
                                    "workspace worker is required for local sessions: {error}"
                                ))
                            })?;
                    let worker_sandbox_mode =
                        atelier_workspace::worker::WorkspaceWorkerSandboxMode::configured()
                            .map_err(|error| {
                                acp::Error::internal_error().data(format!(
                                    "workspace worker sandbox mode is unavailable: {error}"
                                ))
                            })?;
                    let worker_cwd = cwd.to_path_buf();
                    workspace_ops
                        .get_or_init_local_session_filesystem(|| async move {
                            let worker = atelier_workspace::WorkspaceWorkerClient::spawn(
                                worker_cwd.clone(),
                                worker_path,
                                worker_sandbox_mode,
                            )
                            .await?;
                            Ok(Arc::new(atelier_workspace::WorkspaceWorkerFs::new(
                                worker_cwd,
                                worker,
                            )) as Arc<dyn atelier_workspace::file_system::AsyncFileSystem>)
                        })
                        .await
                        .map_err(|error| {
                            acp::Error::internal_error().data(format!(
                                "workspace worker failed to start; refusing local filesystem access: {error}"
                            ))
                        })?
                }
            };
        let gateway_enabled = std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(true),
        );
        let terminal: std::sync::Arc<dyn crate::terminal::AsyncTerminalRunner> = if client_terminal {
            std::sync::Arc::new(AcpTerminalRunner {
                gateway: self.gateway.clone(),
                session_id: session_info.id.clone(),
            })
        } else {
            let notifier: std::sync::Arc<
                dyn crate::terminal::SessionNotificationSender,
            > = std::sync::Arc::new(
                crate::terminal::GatedNotifier::new(
                    std::sync::Arc::new(self.gateway.clone()),
                    gateway_enabled.clone(),
                ),
            );
            std::sync::Arc::new(TerminalRunner::new(notifier, session_info.id.clone()))
        };
        let load_envrc = self.cfg.borrow().session.load_envrc.unwrap_or(true);
        let startup_hints = init
            .meta
            .as_ref()
            .and_then(|m| m.get("startupHints"))
            .and_then(|v| {
                serde_json::from_value::<crate::session::StartupHints>(v.clone()).ok()
            })
            .unwrap_or_default();
        let hunk_plan = plan_hunk_tracking(
            init
                .client_capabilities
                .meta
                .as_ref()
                .and_then(|m| m.get("atelier/hunkTracker"))
                .and_then(|v| v.get("mode"))
                .and_then(|v| v.as_str()),
        );
        let incremental_bash_output = init
            .client_capabilities
            .meta
            .as_ref()
            .and_then(|m| m.get("atelier/incrementalBashOutput"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let no_color = init
            .client_capabilities
            .meta
            .as_ref()
            .and_then(|m| m.get("atelier/bashOutputNoColor"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let hunk_tracking_enabled = hunk_plan.enabled();
        let (hunk_tracker_handle, hunk_event_rx) = match hunk_plan.actor_mode {
            Some(mode) => {
                let cancel = CancellationToken::new();
                let (hunk_event_tx, hunk_event_rx) = tokio::sync::mpsc::unbounded_channel();
                let handle = HunkTrackerActor::spawn(
                    session_info.id.0.to_string(),
                    cwd.as_path().to_path_buf(),
                    hunk_event_tx,
                    mode,
                    cancel.clone(),
                );
                (handle, Some((hunk_event_rx, cancel)))
            }
            None => (atelier_hunk_tracker::HunkTrackerHandle::noop(), None),
        };
        let has_refresh_auth = self
            .auth_manager
            .current()
            .is_some_and(|auth| auth.is_configured_refresh_auth());
        let loc_tracking_enabled = hunk_tracking_enabled && has_refresh_auth
            && (self
                .cfg
                .borrow()
                .local_runtime_settings
                .as_ref()
                .and_then(|s| s.loc_tracking)
                .unwrap_or(false)
                || std::env::var("ATELIER_LOC_TRACKING")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false));
        let (feedback_resolved, feedback_flags) = {
            let cfg = self.cfg.borrow();
            let resolved = cfg.resolve_feedback();
            let flags = crate::session::feedback_manager::FeedbackFlags {
                enabled: resolved.value,
            };
            (resolved, flags)
        };
        tracing::info!(feedback = % feedback_resolved, "resolved feedback feature flag");
        let loc_aggregate_rx = match hunk_event_rx {
            Some((hunk_event_rx, loc_cancel)) if loc_tracking_enabled => {
                let (loc_agg_tx, loc_agg_rx) = tokio::sync::mpsc::unbounded_channel();
                let loc_path = crate::session::persistence::session_dir(&session_info)
                    .join("hunk_records.jsonl");
                let loc_writer = atelier_hunk_tracker::JsonlHunkRecordWriter::new(loc_path);
                let loc_ctx = atelier_hunk_tracker::LocSinkContext {
                    session_id: session_info.id.0.to_string(),
                    agent_id: agent_id(),
                    user_id: self.auth_manager.current().map(|a| a.user_id.clone()),
                    aggregate_tx: Some(loc_agg_tx),
                };
                tokio::spawn(
                    atelier_hunk_tracker::run_loc_sink(
                        hunk_event_rx,
                        loc_writer,
                        loc_ctx,
                        loc_cancel,
                    ),
                );
                Some(loc_agg_rx)
            }
            _ => None,
        };
        let project_env_trusted = folder_trust::project_scope_allowed(cwd.as_path());
        let mut session_env = atelier_workspace::permission::claude_settings::load_claude_env_with_project(
            cwd.as_path(),
            project_env_trusted,
        );
        let envrc = match preloaded_envrc {
            Some(env) => env,
            None => {
                atelier_workspace::envrc::load_envrc_or_empty_when_trusted(
                    cwd.as_path(),
                    load_envrc && project_env_trusted,
                )
            }
        };
        session_env.extend(envrc);
        if no_color {
            session_env.extend(crate::terminal::no_color_env());
        } else {
            session_env.extend(crate::terminal::color_env());
        }
        let workspace_filesystem = fs.clone();
        let mut tool_ctx = ToolContext::with_preloaded_env(
                cwd.clone(),
                Some(self.gateway.clone()),
                Some(session_info.id.clone()),
                fs,
                terminal,
                hunk_tracker_handle,
                session_env,
            )
            .with_hunk_tracking_enabled(hunk_tracking_enabled);
        if workspace_ops.local_session_filesystem().is_none() {
            workspace_ops
                .set_local_session_filesystem(workspace_filesystem)
                .map_err(|error| {
                    acp::Error::internal_error().data(format!(
                        "failed to bind workspace worker filesystem: {error}"
                    ))
                })?;
        }
        tool_ctx.subagent_event_tx = Some(self.subagent_event_tx.clone());
        tool_ctx.runtime_policy = self.policy_engine.clone();
        tool_ctx.runtime_control = Some(self.runtime_control.clone());
        tool_ctx.synthetic_trace_tx = self
            .subagent_coordinator
            .borrow()
            .synthetic_trace_tx
            .clone();
        if let Some(ref shared) = tool_ctx.synthetic_trace_tx_shared {
            *shared.lock().unwrap_or_else(|e| e.into_inner()) = self
                .subagent_coordinator
                .borrow()
                .synthetic_trace_tx
                .clone();
        }
        tool_ctx.is_turn_active = Some(
            self.subagent_coordinator.borrow().turn_active_flag(),
        );
        tool_ctx.monitor_event_buffer = Some(self.monitor_event_buffer.clone());
        tool_ctx.subagent_depth = 0;
        tool_ctx.auto_wake_enabled = self.cfg.borrow().auto_wake_enabled;
        let support_permission = self.cfg.borrow().features.support_permission;
        let telemetry_enabled = self.product_analytics_enabled();
        let origin_client = self.origin_client_info_from_meta(init.meta.as_ref());
        let mut sampling_config = self
            .resolve_sampling_config_for_model(&session_model_id, origin_client.clone());
        if let Some((role_id, role)) = session_role {
            let role_model = format!("{}/{}", role.provider, role.model);
            let role_entry = self
                .resolve_model_id(&acp::ModelId::new(role_model.clone()))
                .map_err(|_| {
                    acp::Error::invalid_params().data(format!(
                        "configured {role_id} role model is unavailable: {role_model}"
                    ))
                })?;
            if !role_entry.info.user_selectable {
                return Err(acp::Error::invalid_params().data(format!(
                    "configured {role_id} role model is not selectable: {role_model}"
                )));
            }
            session_model_id = acp::ModelId::new(role_model);
            sampling_config = self
                .prepare_sampling_config_for_model(&role_entry, origin_client.clone());
            Self::apply_role_to_sampling_config(&mut sampling_config, role_id, &role)?;
        }
        if self.auth_method_id.load().is_none() {
            return Err(acp::Error::auth_required().data("no auth method id provided"));
        }
        let auth_method_id = std::sync::Arc::clone(&self.auth_method_id);
        tracing::info!(
            session_id = % session_info.id.0, ? startup_hints, "startup hints"
        );
        let auto_compact_threshold_percent = {
            let cfg = self.cfg.borrow();
            let models = self.models_manager.models();
            let model = config::find_model_by_id(&models, &session_model_id.0);
            crate::util::config::resolve_auto_compact_threshold_percent(
                &cfg,
                &session_model_id.0,
                model.map(|e| &e.info),
            )
        };
        let system_prompt_label = {
            let cfg = self.cfg.borrow();
            let models = self.models_manager.models();
            let model = config::find_model_by_id(&models, &session_model_id.0);
            crate::util::config::resolve_system_prompt_label(
                &cfg,
                &session_model_id.0,
                model.map(|e| &e.info),
            )
        };
        let compaction_mode = self.cfg.borrow().resolve_compaction_mode();
        let compaction_verbatim_input = self
            .cfg
            .borrow()
            .resolve_compaction_verbatim_input();
        let two_pass_enabled = self.cfg.borrow().is_two_pass_compaction_enabled();
        let auto_update = self.cfg.borrow().cli.auto_update;
        let client_type = *self.client_type.borrow();
        let buffering_settings = self.buffering_settings.borrow().clone();
        tracing::info!(
            session_id = % session_info.id.0,
            "Initializing local feedback manager for session"
        );
        let skills = self.cfg.borrow().skills.clone();
        let compat = self.cfg.borrow().compat_resolved;
        let init_meta = self
            .initialize_request
            .get()
            .and_then(|init| init.meta.as_ref());
        let session_default_agent_profile = builtin_agent_profile_from_meta(
            session_meta,
            init_meta,
        )
        .map_err(|message| acp::Error::invalid_params().data(message))?
        .map(str::to_owned);
        let mut agent_definition = if let Some(profile) = session_default_agent_profile.as_deref() {
            atelier_agent::discovery::by_name(profile).expect("validated built-in Agent harness")
        } else {
            let cfg = self.cfg.borrow();
            Self::resolve_agent_definition(cwd.as_path(), &cfg.agent, model_agent_type)
        };
        {
            let cfg = self.cfg.borrow();
            let overrides = &cfg.cli_agent_overrides;
            overrides.apply_to_definition(&mut agent_definition);
            if overrides.has_definition_overrides() {
                tracing::debug!(
                    agent = % agent_definition.name, tools = ? overrides.tools,
                    disallowed = ? overrides.disallowed_tools, permission_mode = ?
                    overrides.permission_mode, "cli agent overrides applied"
                );
            }
        }
        let pinned_model: Option<(acp::ModelId, ModelEntry)> = match &agent_definition
            .model
        {
            atelier_agent::config::ModelOverride::Override(id) => {
                let mid = acp::ModelId::new(Arc::from(id.as_str()));
                match self.resolve_model_id(&mid) {
                    Ok(entry) => Some((mid, entry)),
                    Err(_) => {
                        tracing::warn!(
                            agent = % agent_definition.name, model = % id,
                            "agent profile model not in catalog, keeping session default"
                        );
                        None
                    }
                }
            }
            atelier_agent::config::ModelOverride::Inherit => None,
        };
        if let Some(template) = inherited_harness_template(
            &agent_definition.user_message_template,
            pinned_model.as_ref().map(|(_, e)| e.info().agent_type.as_str()),
            cwd.as_path(),
        ) {
            tracing::info!(
                agent = % agent_definition.name,
                "Inheriting harness wire-format from the profile model's agent_type"
            );
            agent_definition.user_message_template = template;
        }
        let (session_model_id, sampling_config) = self
            .apply_agent_model_override(
                pinned_model.as_ref(),
                session_model_id,
                sampling_config,
                origin_client.clone(),
            );
        let max_turns = {
            let cfg = self.cfg.borrow();
            cfg.cli_agent_overrides
                .max_turns
                .or(agent_definition.max_turns)
                .map(|v| v as usize)
        };
        {
            let cfg = self.cfg.borrow();
            let effective = cfg
                .toolset
                .resolve_file_toolset(cfg.local_runtime_settings.as_ref());
            if effective != crate::tools::FileToolset::Standard {
                let file_tools = effective
                    .tool_configs(&cfg.toolset.hashline)
                    .map_err(|e| {
                        acp::Error::invalid_params()
                            .data(format!("invalid [toolset.hashline] config: {e}"))
                    })?;
                agent_definition.override_file_tools(file_tools);
            }
        }
        let lsp_tools_enabled = self.cfg.borrow().resolve_lsp_tools().value;
        if lsp_tools_enabled && tool_ctx.lsp.is_none() {
            let snapshot = self.plugin_registry_handle.snapshot();
            let active: Vec<_> = snapshot
                .iter()
                .flat_map(|reg| reg.active_plugins())
                .collect();
            let (plugin_lsp_paths, plugin_names): (Vec<std::path::PathBuf>, Vec<&str>) = active
                .iter()
                .filter_map(|p| {
                    p.lsp_config_path.clone().map(|path| (path, p.name.as_str()))
                })
                .unzip();
            let (
                plugin_inline_lsp,
                inline_names,
            ): (Vec<&serde_json::Value>, Vec<&str>) = active
                .iter()
                .filter_map(|p| {
                    p.inline_lsp_servers.as_ref().map(|v| (v, p.name.as_str()))
                })
                .unzip();
            let sourced = atelier_tools::implementations::lsp::config::load_servers_with_plugins_sourced(
                tool_ctx.cwd.as_path(),
                &plugin_lsp_paths,
                &plugin_inline_lsp,
                &plugin_names,
                &inline_names,
            );
            let servers = folder_trust::filter_untrusted_project_lsp(
                tool_ctx.cwd.as_path(),
                sourced,
            );
            tool_ctx.lsp_server_names = servers.keys().cloned().collect();
            if servers.is_empty() {
                let user_path = atelier_tools::util::atelier_home::atelier_home()
                    .join("lsp.json");
                let project_path = tool_ctx.cwd.as_path().join(".atelier").join("lsp.json");
                tracing::warn!(
                    cwd = % tool_ctx.cwd, user_lsp_path = % user_path.display(),
                    project_lsp_path = % project_path.display(),
                    "LSP tools enabled, but no language servers are configured"
                );
            } else {
                use atelier_tools::implementations::lsp::{
                    LspBackend, LspBackendAdapter, LspManager,
                };
                let mgr = std::sync::Arc::new(
                    tokio::sync::Mutex::new(
                        LspManager::new(
                            servers,
                            tool_ctx.cwd.as_path().to_path_buf(),
                            true,
                            atelier_tools::notification::ToolNotificationHandle::noop(),
                        ),
                    ),
                );
                let adapter = std::sync::Arc::new(LspBackendAdapter::new(mgr));
                adapter.ensure_started_background();
                tool_ctx.lsp = Some(adapter as std::sync::Arc<dyn LspBackend>);
            }
        }
        let inference_idle_timeout_secs = {
            let models = self.models_manager.models();
            let cfg = self.cfg.borrow();
            resolve_inference_idle_timeout_secs(
                &models,
                &sampling_config.model,
                cfg.local_runtime_settings.as_ref(),
            )
        };
        // Transport recovery is an operation-level policy, not model metadata.
        // Unset means the sampler-owned default (currently five retries).
        let configured_max_retries = self.cfg.borrow().retry.max_retries;
        let origin_client = self.origin_client_info_from_meta(init.meta.as_ref());
        let web_search_sampling_config = self.prepare_web_search_sampling_config();
        let image_gen_config = self.prepare_image_gen_config();
        let video_gen_config = self.prepare_video_gen_config();
        let app_builder_deployer_config = self.prepare_app_builder_deployer_config();
        let web_fetch_config = self.prepare_web_fetch_config();
        let write_file_enabled = self.cfg.borrow().resolve_write_file().value;
        let goal_enabled = self.cfg.borrow().resolve_goal().value;
        let subagents_enabled = self.cfg.borrow().subagents_enabled;
        let ask_user_question_enabled = crate::local_artifacts::turn::parse_ask_user_question_from_meta(
                session_meta,
            )
            .unwrap_or_else(|| self.cfg.borrow().resolve_ask_user_question().value);
        let client_hooks = crate::extensions::hooks::parse_client_hooks(session_meta);
        let disable_web_search = self.cfg.borrow().disable_web_search;
        let todo_gate = self.cfg.borrow().todo_gate;
        let laziness_debug_log_for_spawn = self.cfg.borrow().laziness_debug_log.clone();
        let respect_gitignore = self.cfg.borrow().respect_gitignore;
        let path_not_found_hints = self.cfg.borrow().path_not_found_hints;
        let subagent_toggle = self.subagent_toggle.clone();
        let handle_display_cwd = prompt_display_cwd.clone();
        let auth_manager = Some(self.auth_manager.clone());
        let bash_params_json = {
            let cfg = self.cfg.borrow();
            let remote_auto_bg = cfg
                .local_runtime_settings
                .as_ref()
                .and_then(|r| r.auto_background_on_timeout);
            let remote_allow_background_operator = cfg
                .local_runtime_settings
                .as_ref()
                .and_then(|r| r.allow_background_operator);
            cfg.toolset
                .bash
                .to_bash_params_json(remote_auto_bg, remote_allow_background_operator)
        };
        let ask_user_question_params_json = {
            let cfg = self.cfg.borrow();
            let params = crate::util::config::resolve_ask_user_question_params_from_disk(
                cfg.local_runtime_settings.as_ref(),
            );
            match serde_json::to_value(params) {
                Ok(serde_json::Value::Object(map)) => Some(map),
                _ => None,
            }
        };
        let tool_params_json = crate::session::agent_rebuild::ResolvedToolParamsJson {
            bash: Some(bash_params_json),
            ask_user_question: ask_user_question_params_json,
        };
        let backend_tools_enabled = {
            let cfg = self.cfg.borrow();
            cfg.resolve_backend_tools().value
        };
        if let Some(override_prompt) = system_prompt_override_from_meta(
            session_meta,
            init_meta,
        ) && !chat_history.is_empty() && !startup_hints.preserve_inherited_system
        {
            let changed = replace_or_insert_system_head(
                &mut chat_history,
                override_prompt,
            );
            if changed {
                tracing::info!(
                    session_id = % session_info.id.0, prompt_len = override_prompt.len(),
                    "cold-load: applied systemPromptOverride to loaded head"
                );
            } else {
                tracing::debug!(
                    session_id = % session_info.id.0,
                    "cold-load: systemPromptOverride already matches head, no-op"
                );
            }
        }
        let (mut handle, permission_events_rx, agent_system_prompt, session_thread) = {
            let _timer = crate::instrumentation_timer!("session.spawn_actor_call");
            let session_key = self.auth_manager.current_or_expired().map(|a| a.key);
            let credentials = atelier_chat_state::Credentials {
                api_key: sampling_config.api_key.clone(),
                auth_type: crate::agent::config::resolve_chat_state_auth_type(
                    sampling_config.model.as_str(),
                    session_key.as_deref(),
                    self.auth_type(),
                ),
                alpha_test_key: self.alpha_test_key(),
                client_version: sampling_config.client_version.clone(),
            };
            let attribution_callback: Option<
                atelier_sampler::SharedAttributionCallback,
            > = Some(
                crate::auth::attribution::ShellAttribution::new(
                    self.auth_manager.clone(),
                    Some(session_info.id.0.to_string()),
                ),
            );
            let agent_hook_registry_override = agent_definition
                .hooks
                .as_ref()
                .and_then(|hooks_config| {
                    let hooks_val = hooks_config.as_value();
                    let (specs, errors) = atelier_hooks::config::parse_hooks_from_value_with_dir(
                        &hooks_val,
                        &format!("agent:{}", agent_definition.name),
                        std::path::Path::new(&session_info.cwd),
                    );
                    for e in &errors {
                        tracing::warn!(
                            agent = % agent_definition.name, error = ? e,
                            "agent hook parse error"
                        );
                    }
                    if specs.is_empty() {
                        return None;
                    }
                    let cwd = std::path::Path::new(&session_info.cwd);
                    let hooks_trusted = folder_trust::project_scope_allowed(cwd);
                    let git_root = atelier_workspace::session::git::find_git_root_from_path(
                            cwd,
                        )
                        .ok();
                    let (disk_registry, disk_errors) = crate::util::hooks::discover_hooks(
                        git_root.as_deref(),
                        &compat,
                        hooks_trusted,
                    );
                    for e in &disk_errors {
                        tracing::warn!(error = ? e, "hook loading error");
                    }
                    let mut merged = disk_registry;
                    merged.append_specs(specs);
                    Some(std::sync::Arc::new(merged))
                });
            let initial_reasoning_effort = chat_history
                .is_empty()
                .then_some(sampling_config.reasoning_effort);
            let _ = persistence
                .tx
                .send(crate::session::persistence::PersistenceMsg::CurrentModel {
                    model_id: session_model_id.clone(),
                    agent_name: Some(agent_definition.name.clone()),
                    reasoning_effort: initial_reasoning_effort,
                });
            let acp_mcp_servers = crate::session::acp_mcp::parse_acp_mcp_servers(
                session_meta,
            );
            let git_head_changed = init
                .client_capabilities
                .meta
                .as_ref()
                .and_then(|m| m.get("atelier/gitHeadChanged"))
                .and_then(|v| v.as_bool());
            let fs_watch_caps = crate::session::fs_watch::FsWatchCapabilities::resolve(crate::session::fs_watch::CapabilityInputs {
                client_notify: fs_notify_config.is_some(),
                hunk_tracking: hunk_plan.enabled(),
                code_nav: client_code_nav_enabled,
                git_head_changed,
            });
            spawn_session_on_thread(
                    session_info.clone(),
                    self.gateway.clone(),
                    sampling_config,
                    credentials,
                    auth_method_id,
                    auth_manager,
                    attribution_callback,
                    tool_ctx,
                    mcp_servers,
                    initial_client_mcp_servers,
                    mcp_meta_config_map,
                    None,
                    acp_mcp_servers,
                    support_permission,
                    telemetry_enabled,
                    auto_update,
                    persistence,
                    chat_history.clone(),
                    rewind_points_file_path,
                    fs_notify_config,
                    initial_total_tokens,
                    startup_hints,
                    client_type,
                    auto_compact_threshold_percent,
                    system_prompt_label,
                    compaction_mode,
                    compaction_verbatim_input,
                    two_pass_enabled,
                    buffering_settings,
                    origin_client.clone(),
                    self.codebase_indexes.clone(),
                    client_code_nav_enabled,
                    fs_watch_caps,
                    client_terminal,
                    client_fs_read && client_fs_write,
                    gateway_enabled,
                    agent_definition,
                    session_default_agent_profile,
                    skills,
                    None,
                    compat,
                    incremental_bash_output,
                    persisted_signals,
                    persisted_plan_mode,
                    persisted_goal_mode,
                    persisted_announcement_state,
                    self.memory_config.clone(),
                    loc_tracking_enabled,
                    feedback_flags,
                    self.managed_mcp_cache.clone(),
                    managed_mcp_expires_at,
                    session_model_id,
                    session_yolo_mode,
                    session_auto_mode,
                    origin_client.as_ref().map(|o| o.product.clone()),
                    inference_idle_timeout_secs,
                    configured_max_retries,
                    web_search_sampling_config,
                    web_fetch_config,
                    image_gen_config,
                    video_gen_config,
                    app_builder_deployer_config,
                    write_file_enabled,
                    goal_enabled,
                    subagents_enabled,
                    ask_user_question_enabled,
                    client_hooks,
                    prompt_display_cwd,
                    subagent_toggle,
                    atelier_agent::prompt::context::PromptAudience::Primary,
                    None,
                    disable_web_search,
                    backend_tools_enabled,
                    respect_gitignore,
                    path_not_found_hints,
                    tool_params_json,
                    {
                        let session_cwd = std::path::Path::new(&session_info.cwd);
                        let disk_cfg = crate::config::resolve_effective_plugins_config(
                                session_cwd,
                            )
                            .to_discovery_config();
                        self.plugin_registry_handle
                            .refresh_and_build_for_cwd(
                                session_cwd,
                                &disk_cfg,
                                &parse_session_plugin_dirs(session_meta),
                                folder_trust::project_scope_allowed(session_cwd),
                            )
                    },
                    Some(self.plugin_registry_handle.clone()),
                    self.models_manager.clone(),
                    None,
                    Some(
                        Arc::new(
                            crate::auth::manager::SharedAuthKeyProvider(
                                self.auth_manager.clone(),
                            ),
                        ),
                    ),
                    self.resolve_image_description_model(),
                    agent_hook_registry_override,
                    workspace_ops.clone(),
                    {
                        let cfg = self.cfg.borrow();
                        cfg.cli_agent_overrides.permission_rules.clone()
                    },
                    todo_gate,
                    laziness_debug_log_for_spawn,
                    None,
                    None,
                    max_turns,
                    None,
                )
                .await?
        };
        self.session_threads
            .borrow_mut()
            .insert(session_info.id.clone(), session_thread);
        tracing::debug!(
            session_id = % session_info.id.0, "spawn_session_on_thread complete"
        );
        self.set_session_live_state(&session_info.id, SessionLiveState::IdleResident);
        self.ensure_session_supervisor();
        self.heap_profile_set_session_id(&session_info.id.0);
        self.push_roster_delta_upserted(&session_info.id);
        if chat_history.is_empty() {
            let _timer = crate::instrumentation_timer!("session.system_prompt_inject");
            let system_prompt = build_spawn_system_prompt(
                session_meta,
                init_meta,
                &agent_system_prompt,
            );
            tracing::debug!(session_id = % session_info.id.0, "built system prompt");
            let _ = handle
                .cmd_tx
                .send(SessionCommand::Initialize {
                    system_prompt,
                });
            tracing::debug!(
                session_id = % session_info.id.0, "enqueued SessionCommand::Initialize"
            );
        }
        let _ = handle.cmd_tx.send(SessionCommand::AdvertiseCommands);
        if let Some(mut loc_rx) = loc_aggregate_rx {
            let signals = handle.signals_handle.clone();
            tokio::spawn(async move {
                while let Some(agg) = loc_rx.recv().await {
                    match agg {
                        atelier_hunk_tracker::LocAggregate::LinesChanged {
                            author_type,
                            lines_added,
                            lines_removed,
                            file_path,
                        } => {
                            let is_agent = author_type
                                == atelier_hunk_tracker::AuthorType::Agent;
                            signals
                                .record_loc_change(
                                    is_agent,
                                    lines_added,
                                    lines_removed,
                                    file_path,
                                );
                        }
                        atelier_hunk_tracker::LocAggregate::LinesReverted {
                            lines_added_reverted,
                            lines_removed_reverted,
                        } => {
                            signals
                                .record_loc_revert(
                                    lines_added_reverted,
                                    lines_removed_reverted,
                                );
                        }
                    }
                }
            });
        }
        self.permission_event_receivers
            .borrow_mut()
            .insert(session_info.id.clone(), permission_events_rx);
        if handle_display_cwd.is_some() {
            handle.display_cwd = handle_display_cwd;
        }
        let source = if chat_history.is_empty() { "new" } else { "load" };
        let _ = handle
            .cmd_tx
            .send(SessionCommand::DispatchSessionStartHook {
                source: source.to_string(),
            });
        self.notify_session_cwd_for_watch(std::path::Path::new(&session_info.cwd));
        self.activity.register_session(&session_info.id.0, &handle);
        self.sessions.borrow_mut().insert(session_info.id.clone(), handle);
        let cwd_for_maintenance = session_info.cwd.clone();
        tokio::spawn(async move {
            crate::session::prompt_history::truncate_if_needed_async(cwd_for_maintenance)
                .await;
        });
        Ok(())
    }
    /// Collects all pending permission events from a session's receiver.
    /// Returns only the events from the current turn (since last collection).
    pub(super) fn collect_permission_events(
        &self,
        session_id: &acp::SessionId,
    ) -> Vec<PermissionEvent> {
        let mut events = Vec::new();
        if let Some(rx) = self
            .permission_event_receivers
            .borrow_mut()
            .get_mut(session_id)
        {
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
        }
        events
    }
}

#[cfg(test)]
mod role_sampling_tests {
    use super::{MvpAgent, load_configured_role};
    use atelier_provider::{
        CredentialRef, ModelCapabilities, ModelDescriptor, ModelKey, ModelSource, ProviderConfig,
        ProviderDiscovery, ProviderAuth, ProviderRegistry, RoleConfig, RoleId,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn main_role_payload_and_effort_are_kept_in_the_session_snapshot() {
        let mut role = RoleConfig::new("proxy", "model").unwrap();
        role.effort = Some("high".to_owned());
        role.fast_mode = true;
        role.payload.insert("temperature".to_owned(), json!(0.2));
        role.payload.insert("shared".to_owned(), json!("role"));

        let mut config = atelier_sampler::SamplerConfig::default();
        config
            .request_payload
            .insert("provider_only".to_owned(), json!("default"));
        config
            .request_payload
            .insert("shared".to_owned(), json!("provider"));
        MvpAgent::apply_role_to_sampling_config(&mut config, RoleId::Main, &role).unwrap();

        assert_eq!(
            config.reasoning_effort,
            Some(atelier_sampling_types::ReasoningEffort::High)
        );
        assert_eq!(config.request_payload["temperature"], json!(0.2));
        assert!(!config.request_payload.contains_key("fast_mode"));
        assert_eq!(config.request_payload["service_tier"], json!("priority"));
        assert_eq!(config.request_payload["provider_only"], json!("default"));
        assert_eq!(config.request_payload["shared"], json!("role"));
    }

    #[test]
    fn invalid_role_effort_fails_at_session_spawn() {
        let mut role = RoleConfig::new("proxy", "model").unwrap();
        role.effort = Some("unsupported".to_owned());

        let error = MvpAgent::apply_role_to_sampling_config(
            &mut atelier_sampler::SamplerConfig::default(),
            RoleId::Main,
            &role,
        )
        .expect_err("invalid effort must fail closed");
        assert!(error.to_string().contains("unsupported main role effort"));
    }

    #[test]
    fn derived_role_effort_failure_identifies_the_captured_role() {
        let mut role = RoleConfig::new("proxy", "model").unwrap();
        role.effort = Some("unsupported".to_owned());

        let error = MvpAgent::apply_role_to_sampling_config(
            &mut atelier_sampler::SamplerConfig::default(),
            RoleId::Review,
            &role,
        )
        .expect_err("invalid derived Role effort must fail closed");

        assert!(error.to_string().contains("unsupported review role effort"));
    }

    #[test]
    fn derived_role_snapshot_applies_effort_fast_mode_and_payload() {
        let mut role = RoleConfig::new("example", "review-model").unwrap();
        role.effort = Some("low".to_owned());
        role.fast_mode = true;
        role.payload.insert("temperature".to_owned(), json!(0.15));

        let mut config = atelier_sampler::SamplerConfig::default();
        MvpAgent::apply_role_to_sampling_config(&mut config, RoleId::Review, &role).unwrap();

        assert_eq!(
            config.reasoning_effort,
            Some(atelier_sampling_types::ReasoningEffort::Low)
        );
        assert!(!config.request_payload.contains_key("fast_mode"));
        assert_eq!(config.request_payload["service_tier"], json!("priority"));
        assert_eq!(config.request_payload["temperature"], json!(0.15));
    }

    #[test]
    fn invalid_role_registry_is_reported_instead_of_falling_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("providers.toml");
        std::fs::write(&path, "schema_version = 999\n").unwrap();

        let main = RoleConfig::new("provider", "model").unwrap();
        let error = load_configured_role(
            &path,
            atelier_provider::RoleId::Main,
            &main,
            false,
        )
        .expect_err("invalid role registry must fail closed");

        assert!(
            error
                .to_string()
                .contains("failed to load configured main role")
        );
    }

}
