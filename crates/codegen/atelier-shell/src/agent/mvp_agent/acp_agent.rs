#![cfg_attr(rustfmt, rustfmt::skip)]
#![allow(unused_imports)]
//! [`acp::Agent`] trait implementation for [`MvpAgent`].
//! Co-located child of `mvp_agent` (`use super::*`).
use super::*;
#[async_trait::async_trait(?Send)]
impl acp::Agent for MvpAgent {
    /// In the meta, we provide
    ///   - model_state: the model state, useful for the client to display available models and the default model.
    ///
    /// SINGLE-CALL INVARIANT: this method is the sole writer of
    /// `self.auth_method_id` during initialization. It is called exactly once
    /// per agent process by the ACP server before any session-creating
    /// requests, while `auth_method_id` is still `None` (initialized at
    /// `MvpAgent::new`). The auth-method block below relies on that
    /// invariant when it unconditionally writes the default id returned by
    /// `auth_method::build_auth_methods`. If you ever need to call
    /// `initialize()` more than once, restore an `is_none()` guard around
    /// the `auth_method_id` write at the call site so a re-init doesn't
    /// silently downgrade an api-key user to a session-token user.
    async fn initialize(
        &self,
        arguments: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, acp::Error> {
        tracing::debug!(target : "sampling_log", "Received initialize request");
        atelier_telemetry::unified_log::info("agent initialized", None, None);
        self.start_subagent_coordinator();
        tokio::task::spawn_blocking(|| {
            crate::session::worktree_pool::cleanup_stale_pool_worktrees(None);
        });
        tokio::task::spawn_blocking(|| {
            crate::session::persistence::cleanup_stale_sessions(None);
        });
        {
            let root = crate::util::atelier_home::atelier_home();
            crate::session::storage::search::SEARCH_INDEX_MANAGER.bootstrap_once(root);
        }
        const PERMISSION_CLEANUP_TTL_DAYS: u64 = 30;
        static CLEANUP_PERMISSIONS_ONCE: std::sync::Once = std::sync::Once::new();
        CLEANUP_PERMISSIONS_ONCE
            .call_once(|| {
                tokio::task::spawn(
                    atelier_workspace::permission::cleanup_stale_permission_state(
                        std::time::Duration::from_secs(
                            PERMISSION_CLEANUP_TTL_DAYS * 24 * 60 * 60,
                        ),
                    ),
                );
            });
        atelier_workspace::trust::migrate_legacy_hook_trust();
        let mut client_type = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("clientType"))
            .and_then(|v| serde_json::from_value::<ClientType>(v.clone()).ok())
            .unwrap_or_default();
        let client_identifier = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("clientIdentifier"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(ref id) = client_identifier {
            tracing::info!("Client identifier set to: {}", id);
        }
        if client_type == ClientType::Generic {
            match client_identifier.as_deref() {
                Some("atelier-web") => client_type = ClientType::AtelierWeb,
                Some("nebula") => client_type = ClientType::Nebula,
                Some("atelier-code-extension") => client_type = ClientType::Extension,
                Some("atelier-desktop") => client_type = ClientType::Desktop,
                _ => {}
            }
        }
        *self.client_type.borrow_mut() = client_type;
        tracing::info!("Client type set to: {:?}", client_type);
        let code_nav_enabled = Self::parse_code_nav_capability(&arguments);
        self.code_nav_enabled.set(code_nav_enabled);
        tracing::info!(
            code_nav_enabled, client_type = ? client_type, event =
            "code_nav_capability_parsed",
            "code-nav capability initialized from initialize request; \
             index will start lazily on first atelier/code/* request if eligible"
        );
        let interactive_trust_client = Self::parse_interactive_trust_capability(
            &arguments,
        );
        self.interactive_trust_client.set(interactive_trust_client);
        let client_supports_mcp_apps = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("mcpApps"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if client_supports_mcp_apps {
            tracing::info!("Client supports MCP Apps");
        }
        let buffering_settings = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("bufferingSettings"))
            .map(|value| serde_json::from_value::<
                update_chunk_merge::BufferingSettings,
            >(value.clone()))
            .transpose()
            .map_err(|err| {
                tracing::warn!(
                    error = ? err, "Failed to parse buffering settings from init meta"
                );
                err
            })
            .unwrap_or(None);
        tracing::info!(? buffering_settings, "Buffering settings from init");
        *self.buffering_settings.borrow_mut() = buffering_settings;
        if self.initialize_request.set(arguments).is_err() {
            tracing::info!("Initialize called on reconnect (already initialized)");
        }
        let pre = self.auth_manager.current();
        self.auth_manager.force_reload_from_disk();
        let post = self.auth_manager.current();
        atelier_telemetry::unified_log::info(
            "auth init disk refresh",
            None,
            Some(
                serde_json::json!(
                    { "had_access_token" : pre.as_ref().is_some_and(| a | !a.key.is_empty()),
                    "had_refresh_token" : pre.as_ref().is_some_and(| a | a.refresh_token.is_some()),
                    "has_access_token" : post.as_ref().is_some_and(| a | !a.key.is_empty()),
                    "has_refresh_token" : post.as_ref().is_some_and(| a | a.refresh_token.is_some()),
                    "access_token_changed" : pre.as_ref().map(| a | &a.key) != post.as_ref().map(| a | &a.key),
                    "refresh_token_changed" : pre.as_ref().and_then(| a | a.refresh_token.as_ref())
                    != post.as_ref().and_then(| a | a.refresh_token.as_ref()), }
                ),
            ),
        );
        atelier_telemetry::unified_log::info(
            "auth: initialize() refreshed auth state from disk",
            None,
            Some(
                serde_json::json!(
                    { "has_current" : self.auth_manager.current().is_some(), "is_expired"
                    : self.auth_manager.is_expired(), "auth_mode" : self.auth_manager
                    .current().map(| a | format!("{:?}", a.auth_mode)), }
                ),
            ),
        );
        // Provider credentials are resolved from explicit Provider
        // references. Never import an API key from the legacy auth store.
        let disable_api_key_auth = self
            .cfg
            .borrow()
            .atelier_com_config
            .api_key_auth_disabled();
        {
            let cfg = self.cfg.borrow();
            let gc = &cfg.atelier_com_config;
            if disable_api_key_auth || gc.force_login_team_uuid.is_some() {
                atelier_telemetry::unified_log::info(
                    "auth: enterprise login policy active",
                    None,
                    Some(
                        serde_json::json!(
                            { "force_login_team_uuid" : gc.force_login_team_uuid.as_ref()
                            .map(| t | format!("{t:?}")), "disable_api_key_auth_knob" :
                            gc.disable_api_key_auth, "api_key_auth_disabled" :
                            disable_api_key_auth, }
                        ),
                    ),
                );
            }
        }
        let has_external_api_key = auth_method::should_advertise_provider_api_key(
            disable_api_key_auth,
            self.models_manager.models().values(),
        );
        let init_has_current = self.auth_manager.current().is_some();
        let init_is_expired = self.auth_manager.is_expired();
        atelier_telemetry::unified_log::info(
            "auth init token state",
            None,
            Some(
                serde_json::json!(
                    { "has_current" : init_has_current, "is_expired" : init_is_expired, }
                ),
            ),
        );
        let mut has_cached_token = init_has_current;
        if !init_has_current && init_is_expired {
            let refreshed = self.auth_manager.auth().await.is_ok();
            if refreshed {
                tracing::debug!(
                    auth_type = ? self.auth_type(),
                    "auth: initialize() silent refresh succeeded",
                );
                atelier_telemetry::unified_log::info(
                    "auth: initialize() silent refresh succeeded",
                    None,
                    Some(
                        serde_json::json!(
                            { "auth_type" : format!("{:?}", self.auth_type()) }
                        ),
                    ),
                );
                has_cached_token = true;
            } else {
                tracing::warn!(
                    "auth: token expired, silent refresh failed - re-authentication required"
                );
                atelier_telemetry::unified_log::warn(
                    "auth: token expired, silent refresh failed - re-authentication required",
                    None,
                    None,
                );
            }
        }
        // The vendor authentication configuration is intentionally ignored in
        // the Provider runtime. Keeping these values empty also prevents a
        // stale `[atelier_com_config.oidc]` block from advertising a login flow.
        let (login_label, has_auth_provider, has_enterprise_oidc, enterprise_oidc_issuer):
            (Option<String>, bool, bool, Option<String>) = (None, false, false, None);
        if has_enterprise_oidc {
            let issuer = enterprise_oidc_issuer
                .as_deref()
                .expect(
                    "enterprise_oidc_issuer must be Some when has_enterprise_oidc is true",
                );
            tracing::info!(
                issuer = % issuer, "auth: advertising enterprise OIDC auth method",
            );
            atelier_telemetry::unified_log::info(
                "auth: advertising enterprise OIDC auth method",
                None,
                Some(serde_json::json!({ "issuer" : issuer })),
            );
        } else {
            tracing::info!(
                label = ? login_label, has_auth_provider,
                "auth: advertising atelier.invalid auth method",
            );
        }
        let preferred_method = self.cfg.borrow().atelier_com_config.preferred_method;
        let has_external_api_key = match preferred_method {
            Some(crate::auth::PreferredAuthMethod::Oidc) => false,
            _ => has_external_api_key,
        };
        let has_cached_token = match preferred_method {
            Some(crate::auth::PreferredAuthMethod::ApiKey) => false,
            _ => has_cached_token,
        };
        let has_local_provider = self
            .models_manager
            .models()
            .keys()
            .any(|model_id| model_id.contains('/'));
        let built = auth_method::build_auth_methods(auth_method::AuthMethodsBuildInputs {
            vendorless: true,
            has_local_provider,
            has_external_api_key,
            has_cached_token,
            has_enterprise_oidc,
            enterprise_oidc_issuer: enterprise_oidc_issuer.as_deref(),
            login_label: login_label.as_deref(),
            has_auth_provider_command: has_auth_provider,
            preferred_method,
        });
        let auth_methods = built.methods;
        atelier_telemetry::unified_log::info(
            "auth: initialize() built auth_methods for ACP response",
            None,
            Some(
                serde_json::json!(
                    { "atelier_home" : crate ::util::atelier_home::atelier_home().display()
                    .to_string(), "HOME" : std::env::var("HOME").unwrap_or_else(| _ |
                    "(unset)".into()), "has_external_api_key" : has_external_api_key,
                    "disable_api_key_auth" : disable_api_key_auth, "has_cached_token" :
                    has_cached_token, "has_enterprise_oidc" : has_enterprise_oidc,
                    "init_has_current" : init_has_current, "init_is_expired" :
                    init_is_expired, "auth_mode" : self.auth_manager.current().map(| a |
                    format!("{:?}", a.auth_mode)), "methods" : auth_methods.iter().map(|
                    m | m.id().0.as_ref()).collect::< Vec < _ >> (),
                    "default_auth_method_id" : built.default_auth_method_id.as_ref()
                    .map(| id | id.0.as_ref()), }
                ),
            ),
        );
        debug_assert!(
            matches!(auth_methods.first().map(|m|
                auth_method::AuthMethodKind::from_id(m.id())),
                Some(auth_method::AuthMethodKind::LocalProvider)),
            "vendorless invariant violated: atelier.provider MUST be \
             auth_methods.first(); got {:?}",
            auth_methods.first().map(| m | m.id()),
        );
        let default_auth_method_id_wire: Option<String> = built
            .default_auth_method_id
            .as_ref()
            .map(|id| id.0.to_string());
        if let Some(default_id) = built.default_auth_method_id {
            atelier_telemetry::unified_log::info(
                "auth method selection",
                None,
                Some(
                    serde_json::json!(
                        { "default_auth_method_id" : default_id.0.as_ref(),
                        "has_external_api_key" : has_external_api_key, "has_cached_token"
                        : has_cached_token, "methods_first" : auth_methods.first().map(|
                        m | m.id().0.as_ref()), "methods_count" : auth_methods.len(), }
                    ),
                ),
            );
            self.set_auth_method(default_id);
        }
        let current_working_directory = self.launch_cwd.clone();
        let hostname = gethostname::gethostname();
        let mcp_servers: Vec<crate::extensions::mcp::McpServerEntry> = Vec::new();
        self.spawn_initialize_launch_mcp_setup();
        self.spawn_heap_profile_monitor();
        let init_model_state = self.model_state(None);
        Ok(
            acp::InitializeResponse::new(acp::ProtocolVersion::V1)
                .agent_capabilities(
                    acp::AgentCapabilities::new()
                        .load_session(true)
                        .meta(
                            serde_json::json!(
                                { "atelier/fs_notify" : true, "atelier/hooks" : { "blockingEvents"
                                : [atelier_hooks::event::HookEventName::PreToolUse],
                                "decisions" : ["deny"], },
                                "atelier/runtime" : { "version" :
                                atelier_acp_runtime::ATELIER_PROTOCOL_VERSION, "capabilities" :
                                atelier_acp_runtime::ATELIER_PROTOCOL_CAPABILITIES, }, }
                            )
                                .as_object()
                                .cloned(),
                        )
                        .prompt_capabilities(
                            acp::PromptCapabilities::new().embedded_context(true),
                        )
                        .mcp_capabilities(
                            acp::McpCapabilities::new().http(true).sse(true),
                        ),
                )
                .auth_methods(auth_methods)
                .meta({
                    let metadata = parse_json_object_env("ATELIER_AGENT_METADATA");
                    serde_json::json!(
                        { "atelierShell" : true, "defaultAuthMethodId" :
                        default_auth_method_id_wire, (atelier_mcp::wire::MCP_SDK) :
                        true, (SESSION_PLUGIN_DIRS_CAPABILITY_KEY) : true,
                        "currentWorkingDirectory" : current_working_directory
                        .to_string_lossy().to_string(), "agentVersion" :
                        atelier_version::VERSION, "agentId" : agent_id(),
                        "agentInstanceId" : agent_instance_id(), "hostname" : hostname
                        .to_string_lossy().to_string(), "modelState" : init_model_state,
                        "mcpServers" : mcp_servers, "mcpApps" : client_supports_mcp_apps,
                        "metadata" : metadata, "availableCommands" : crate
                        ::session::slash_commands::builtin_commands(self
                        .command_availability()), "cancelRewind" : self.cfg.borrow()
                        .resolve_cancel_rewind().value, "sessionRecap" : self.cfg
                        .borrow().is_session_recap_enabled(), "voiceMode" : self.cfg
                        .borrow().is_voice_mode_enabled(), "atelierRuntime" : {
                        "version" : atelier_acp_runtime::ATELIER_PROTOCOL_VERSION,
                        "capabilities" : atelier_acp_runtime::ATELIER_PROTOCOL_CAPABILITIES, }, }
                    )
                        .as_object()
                        .cloned()
                }),
        )
    }
    async fn authenticate(
        &self,
        arguments: acp::AuthenticateRequest,
    ) -> Result<AuthenticateResponse, acp::Error> {
        if !vendorless_auth_method_allowed(arguments.method_id.0.as_ref()) {
            tracing::warn!(
                method = %arguments.method_id.0,
                "auth: rejected non-local authentication method"
            );
            return Err(acp::Error::auth_required().data(
                "Atelier only accepts atelier.provider; configure credentials on a Provider.",
            ));
        }

        self.set_auth_method(arguments.method_id);
        tracing::debug!("auth: local Provider method accepted without login");
        Ok(AuthenticateResponse::new())
    }
    async fn new_session(
        &self,
        arguments: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, acp::Error> {
        tracing::debug!(
            config = ? self.sampling_config, "Received new session request {arguments:?}"
        );
        let init = self
            .initialize_request
            .get()
            .ok_or_else(|| {
                acp::Error::invalid_params()
                    .data("initialize must be called before new_session")
            })?;
        self.seed_client_config_auth_if_available();
        let cwd = AbsPathBuf::new(arguments.cwd.clone())
            .map_err(|e| acp::Error::invalid_params().data(e.to_string()))?;
        folder_trust::resolve_and_record(cwd.as_path(), None, false);
        let initial_client_mcp_servers = arguments.mcp_servers.clone();
        let (mcp_servers, managed_mcp_expires_at) = self
            .resolve_mcp_servers(arguments.mcp_servers, cwd.as_path())
            .await;
        let mcp_meta_config_map = parse_mcp_meta_config(arguments.meta.as_ref());
        let client_session_id = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("sessionId"))
            .and_then(|v| v.as_str());
        let custom_model_id = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("modelId").and_then(|v| v.as_str()))
            .filter(|s| !s.is_empty());
        #[allow(unused_variables)]
        let session_computer_sessions = parse_session_computer_sessions(
            arguments.meta.as_ref(),
        );
        let is_chat_kind = is_chat_session_kind(arguments.meta.as_ref());
        let session_yolo_mode = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("yoloMode"))
            .and_then(|v| v.as_bool())
            .unwrap_or(self.default_yolo_mode);
        let session_auto_mode = resolve_session_auto_mode(
            arguments.meta.as_ref(),
            self.default_auto_mode,
            session_yolo_mode,
        );
        let session_id = match client_session_id {
            Some(s) => {
                uuid::Uuid::try_parse(s)
                    .map_err(|e| {
                        acp::Error::invalid_params()
                            .data(
                                format!(
                                    "Invalid UUID format for _meta.sessionId '{}': {}", s, e
                                ),
                            )
                    })?;
                acp::SessionId::new(s.to_string())
            }
            None => acp::SessionId::new(uuid::Uuid::now_v7().to_string()),
        };
        let mut session_timer = crate::instrumentation_timer!("session.new_session");
        session_timer.with_field("session_id", session_id.0.as_ref());
        session_timer.with_field("cwd", cwd.as_str());
        let client_identifier = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("clientIdentifier"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                self
                    .initialize_request
                    .get()
                    .and_then(|req| req.meta.as_ref())
                    .and_then(|m| m.get("clientIdentifier"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        atelier_telemetry::session_ctx::log_session_event(crate::agent::session_metrics::SessionStarted {
            session_id: session_id.0.to_string(),
        });
        let session_info = SessionInfo {
            id: session_id.clone(),
            cwd: cwd.as_str().to_owned(),
        };
        let mut session_role = role_for_new_session(arguments.meta.as_ref(), is_chat_kind, |role| {
            self.required_role(role)
        })?;
        let session_role_id = session_role.as_ref().map(|(role_id, _)| *role_id);
        let mut model_agent_type: Option<String> = None;
        let mut session_sampling_override: Option<SamplingConfig> = None;
        let session_initial_model = chat_initial_model(is_chat_kind, custom_model_id);
        let cli_default_model = self.models_manager.cli_default_model_override();
        let persisted_default_model = if !is_chat_kind
            && custom_model_id.is_none()
            && session_role.is_none()
            && cli_default_model.is_none()
        {
            atelier_config::runtime_defaults::resolve_runtime_defaults_at(
                &atelier_config::atelier_home(),
            )
            .map_err(|error| {
                acp::Error::invalid_params().data(format!(
                    "invalid new-session default model configuration: {error}"
                ))
            })?
            .model
        } else {
            None
        };
        let configured_default_model = cli_default_model.or_else(|| persisted_default_model.clone());
        let build_custom_model_id = if is_chat_kind {
            None
        } else {
            custom_model_id.or(configured_default_model.as_deref())
        };
        if build_custom_model_id.is_some()
            && session_role_id.is_some_and(|role| role != atelier_provider::RoleId::Main)
        {
            return Err(acp::Error::invalid_params().data(
                "derived Agent models are fixed by their Role snapshot and cannot be overridden",
            ));
        }
        let resolved_custom_model = match build_custom_model_id {
            Some(custom_model) => {
                let model = self
                    .resolve_model_id(&acp::ModelId::new(custom_model))
                    .map_err(|_| {
                        if persisted_default_model.is_some() {
                            acp::Error::invalid_params()
                                .data(configured_model_unavailable_message(custom_model))
                        } else {
                            acp::Error::invalid_params()
                                .data(format!("requested model is unavailable: {custom_model}"))
                        }
                    })?;
                if !model.info.user_selectable {
                    let message = if persisted_default_model.is_some() {
                        configured_model_unavailable_message(custom_model)
                    } else {
                        format!("requested model is not selectable: {custom_model}")
                    };
                    return Err(acp::Error::invalid_params().data(message));
                }
                model_agent_type = Some(model.info().agent_type.clone());
                let origin_client = self.origin_client_info_from_meta(arguments.meta.as_ref());
                session_sampling_override =
                    Some(self.prepare_sampling_config_for_model(&model, origin_client));
                if let Some((_, role)) = session_role.as_mut() {
                    let key = atelier_provider::ModelKey::parse(custom_model)
                        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
                    role.provider = key.provider_id;
                    role.model = key.model_id;
                }
                Some(custom_model)
            }
            None => None,
        };
        let mut resolved_role_model: Option<String> = None;
        if !is_chat_kind && build_custom_model_id.is_none()
            && let Some((role_id, role)) = session_role.as_ref()
        {
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
            model_agent_type = Some(role_entry.info().agent_type.clone());
            let origin_client = self.origin_client_info_from_meta(arguments.meta.as_ref());
            session_sampling_override = Some(
                self.prepare_sampling_config_for_model(&role_entry, origin_client),
            );
            resolved_role_model = Some(role_model);
        }
        if model_agent_type.is_none() && build_custom_model_id.is_none()
            && let Ok(default_model) = self
                .resolve_model_id(&self.models_manager.current_model_id())
        {
            model_agent_type = Some(default_model.info().agent_type.clone());
        } else if model_agent_type.is_none() && build_custom_model_id.is_some() {
            tracing::debug!(
                custom_model = ? build_custom_model_id, current_model_id = % self
                .models_manager.current_model_id().0,
                "Skipping current_model_id agent_type fallback: custom model was requested, \
                 avoiding cross-client agent_type contamination in leader mode"
            );
        }
        let origin_client = self.origin_client_info_from_meta(arguments.meta.as_ref());
        let mut session_sampling = session_sampling_override
            .unwrap_or_else(|| {
                self
                    .resolve_sampling_config_for_model(
                        &self.models_manager.current_model_id(),
                        origin_client.clone(),
                    )
            });
        if let Some((role_id, role)) = session_role.as_ref()
            && let Some(effort) = role.effort.as_deref()
        {
            session_sampling.reasoning_effort = Some(
                effort.parse().map_err(|_| {
                    acp::Error::invalid_params().data(format!(
                        "unsupported {role_id} role effort: {effort}"
                    ))
                })?,
            );
        }
        if let Some((_, role)) = session_role.as_ref() {
            session_sampling.request_payload = atelier_provider::merge_payloads(
                &session_sampling.request_payload,
                &role.effective_payload(),
            );
        }
        if session_role.is_none()
            && let Some(effort) = self.models_manager.current_reasoning_effort()
            && self
                .models_manager
                .model_supports_reasoning_effort(&session_sampling.model)
        {
            session_sampling.reasoning_effort = Some(effort);
        }
        let title_backend =
            title_backend_for_session(self.build_summary_client(&session_sampling));
        let model_id = match &session_initial_model {
            Some(chat_model) => acp::ModelId::new(chat_model.clone()),
            None => {
                resolved_custom_model
                    .map(str::to_owned)
                    .or(resolved_role_model)
                    .map(acp::ModelId::new)
                    .unwrap_or_else(|| acp::ModelId::new(String::new()))
            }
        };
        let session_model_id = model_id.clone();
        let persistence = if is_chat_kind {
            crate::session::persistence::PersistenceHandle::noop()
        } else {
            let _timer = crate::instrumentation_timer!("session.persistence_init");
            let registry_title_sync = self
                .local_session_catalog()
                .map(|client| crate::session::persistence::RegistryGeneratedTitleSync {
                    client,
                    suppress_for_zdr: self
                        .auth_manager
                        .current_or_expired()
                        .is_some_and(|a| a.is_zdr_team()),
                });
            crate::session::persistence::new(
                    &session_info,
                    model_id,
                    title_backend,
                    self.storage_mode,
                    Some(self.auth_manager.clone()),
                    Some(self.gateway.clone()),
                    registry_title_sync,
                )
                .await
                .map_err(|e| crate::session::persistence::io_error_to_acp(&e))?
        };
        self.session_turn_numbers.borrow_mut().insert(session_id.clone(), 0u64);
        let chat_history = vec![];
        let client_code_nav_enabled = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("codeNavEnabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| self.code_nav_enabled.get());
        let (client_terminal, client_fs_read, client_fs_write) = Self::resolve_client_io_caps(
            arguments.meta.as_ref(),
            init,
        );
        let spawn_res = {
            let mut timer = crate::instrumentation_timer!("session.spawn_session_actor");
            timer.with_field("session_id", session_id.0.as_ref());
            let spawn_opts = if is_chat_kind {
                chat_session_spawn_options(
                    session_info.clone(),
                    cwd.clone(),
                    arguments.meta.as_ref(),
                    model_agent_type.as_deref(),
                    session_model_id,
                    session_yolo_mode,
                )
            } else {
                SessionSpawnOptions {
                        session_info: session_info.clone(),
                        cwd: cwd.clone(),
                        mcp_servers,
                        initial_client_mcp_servers,
                        mcp_meta_config_map,
                        persistence,
                        chat_history,
                        rewind_points_file_path: None,
                        initial_total_tokens: 0,
                        origin_client: origin_client.clone(),
                        client_code_nav_enabled,
                        client_terminal,
                        client_fs_read,
                        client_fs_write,
                        preloaded_envrc: None,
                        persisted_signals: None,
                        persisted_plan_mode: None,
                        persisted_goal_mode: None,
                        persisted_announcement_state: None,
                        session_meta: arguments.meta.as_ref(),
                        managed_mcp_expires_at,
                        model_agent_type: model_agent_type.as_deref(),
                        session_model_id,
                        session_role: session_role.clone(),
                        session_yolo_mode,
                        session_auto_mode: session_auto_mode && !session_yolo_mode,
                        prompt_display_cwd: None,
                    }
            };
            self.spawn_and_register_session(init, spawn_opts).await
        };
        spawn_res?;
        tracing::debug!(session_id = % session_id.0, "new_session: spawn_session_actor");
        self.maybe_spawn_interactive_trust_prompt(
            &session_id,
            cwd.as_path(),
            None,
        );
        let bridge_attach = BridgeAttach::NotAttached;
        let product_analytics = self.product_analytics_enabled();
        if product_analytics {
            let sid = session_id.0.to_string();
            let ci = client_identifier.clone();
            let cv = self.client_version();
            let cwd_str = cwd.as_str().to_owned();
            let perm = if session_yolo_mode {
                atelier_telemetry::enums::PermissionMode::AlwaysApprove
            } else if session_auto_mode
                && crate::util::config::auto_permission_mode_enabled_from_disk()
            {
                atelier_telemetry::enums::PermissionMode::Auto
            } else {
                atelier_telemetry::enums::PermissionMode::Ask
            };
            tokio::spawn(async move {
                let git = atelier_telemetry::context::collect_git_context(&cwd_str);
                let ev = atelier_telemetry::events::SessionNew {
                    session_id: sid,
                    client_identifier: ci,
                    client_version: cv,
                    is_git_repo: git.is_git_repo,
                    permission_mode: perm,
                };
                atelier_telemetry::session_ctx::log_event_dual(product_analytics, ev);
            });
        }
        if let Some(model_id) = resolved_custom_model {
            let _ = crate::timed!(
                log : "new_session: set_session_model", { crate
                ::agent::handlers::model_switch::apply(self,
                acp::SetSessionModelRequest::new(session_id.clone(),
                acp::ModelId::new(model_id)),). await }
            );
            tracing::debug!(
                session_id = % session_id.0, "new_session: set_session_model"
            );
        }
        let indexed_roots = self.indexed_roots_for(cwd.as_path());
        let (git_root, is_git_repo, discovery_failed) = match atelier_workspace::session::git::discover_git_root(
            cwd.as_path(),
        ) {
            GitDiscoveryResult::Found(root) => {
                let root_str = root.to_string_lossy().trim_end_matches('/').to_string();
                (Some(root_str), true, false)
            }
            GitDiscoveryResult::NotARepo => {
                tracing::debug!("new_session: not a git repository");
                (None, false, false)
            }
            GitDiscoveryResult::DiscoveryFailed(e) => {
                tracing::warn!(
                    error = % e, cwd = % cwd.as_str(),
                    "new_session: git repo discovery failed unexpectedly"
                );
                (None, false, true)
            }
        };
        let (show_non_git_warning, feedback_enabled) = {
            let cfg = self.cfg.borrow();
            let show_non_git_warning = !is_git_repo && !discovery_failed
                && cfg
                    .local_runtime_settings
                    .as_ref()
                    .and_then(|s| s.non_git_warning)
                    .unwrap_or(cfg.features.non_git_warning);
            let feedback_enabled = cfg.is_feedback_enabled();
            (show_non_git_warning, feedback_enabled)
        };
        atelier_telemetry::unified_log::info(
            "session created",
            Some(session_id.0.as_ref()),
            Some(serde_json::json!({ "cwd" : cwd.as_str() })),
        );
        let models = self.model_state(Some(&session_id));
        let (session_config_value, session_detail_value) = self
            .session_config_meta(&session_id, cwd.as_str().to_owned(), None, &models);
        let mut meta = serde_json::json!(
            { "currentWorkingDirectory" : cwd.as_str().to_owned(), "codebaseIndexed" :
            indexed_roots, "isGitRepo" : is_git_repo, "gitRoot" : git_root,
            "showNonGitWarning" : show_non_git_warning, "feedbackEnabled" :
            feedback_enabled, }
        );
        if let Some(obj) = meta.as_object_mut() {
            obj.insert("atelier/sessionConfig".to_string(), session_config_value);
            obj.insert("atelier/sessionDetail".to_string(), session_detail_value);
        }
        Ok(
            acp::NewSessionResponse::new(session_id)
                .models(Some(models))
                .meta(meta.as_object().cloned()),
        )
    }
    async fn load_session(
        &self,
        arguments: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        let _load_guard = self.begin_session_load(&arguments.session_id);
        self.sweep_dead_sessions();
        self.drain_old_session_thread(&arguments.session_id).await;
        tracing::debug!("Received load session request {arguments:?}");
        let init = self
            .initialize_request
            .get()
            .ok_or_else(|| {
                acp::Error::invalid_params()
                    .data("initialize must be called before load_session")
            })?;
        self.seed_client_config_auth_if_available();
        let persist_data = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("atelier/persist"))
            .cloned();
        let target_client_id = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("atelier/leaderClientId"))
            .cloned();
        let acp::LoadSessionRequest {
            session_id,
            cwd,
            mcp_servers: client_mcp_servers,
            meta: request_meta,
            ..
        } = arguments;
        let cwd = AbsPathBuf::new(cwd)
            .map_err(|e| acp::Error::invalid_params().data(e.to_string()))?;
        let local_runtime_settings = self.cfg.borrow().local_runtime_settings.clone();
        folder_trust::resolve_and_record(cwd.as_path(), local_runtime_settings.as_ref(), false);
        let initial_client_mcp_servers = client_mcp_servers.clone();
        let (mcp_servers, managed_mcp_expires_at) = self
            .resolve_mcp_servers(client_mcp_servers, cwd.as_path())
            .await;
        let mcp_meta_config_map = parse_mcp_meta_config(request_meta.as_ref());
        let mut load_timer = crate::instrumentation_timer!("session.load_session");
        load_timer.with_field("session_id", session_id.0.as_ref());
        load_timer.with_field("cwd", cwd.as_str());
        let git_root = atelier_workspace::session::git::find_git_root_from_path(
                cwd.as_path(),
            )
            .ok();
        if let Some(root) = git_root {
            tokio::task::spawn_blocking(move || {
                crate::session::worktree_pool::cleanup_stale_pool_worktrees(Some(&root));
            });
        }
        atelier_telemetry::session_ctx::log_session_event(crate::agent::session_metrics::SessionStarted {
            session_id: session_id.0.to_string(),
        });
        let session_info = SessionInfo {
            id: session_id.clone(),
            cwd: cwd.as_str().to_owned(),
        };
        let current_session_dir = crate::session::persistence::session_dir(
            &session_info,
        );
        tokio::task::spawn_blocking(move || {
            crate::session::persistence::cleanup_stale_sessions(
                Some(&current_session_dir),
            );
        });
        let session_exists = self.sessions.borrow().contains_key(&session_id);
        if session_exists {
            tracing::info!(
                session_id = % session_id.0,
                "Reconnect detected: flushing persistence buffer before replay"
            );
            if let Some(handle) = self.sessions.borrow().get(&session_id) {
                handle
                    .gateway_enabled
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
            let mut flush_timer = crate::instrumentation_timer!(
                "session.reconnect_flush"
            );
            flush_timer.with_field("session_id", session_id.0.as_ref());
            if let Err(reason) = self.flush_session(&session_id).await {
                tracing::warn!(
                    session_id = % session_id.0, reason, "Reconnect flush failed"
                );
            }
            drop(flush_timer);
        }
        let origin_client = self.origin_client_info_from_meta(request_meta.as_ref());
        let load_session_sampling = self
            .resolve_sampling_config_for_model(
                &self.models_manager.current_model_id(),
                origin_client.clone(),
            );
        let title_backend =
            title_backend_for_session(self.build_summary_client(&load_session_sampling));
        let mut persistence_timer = crate::instrumentation_timer!("session.load_light");
        persistence_timer.with_field("session_id", session_id.0.as_ref());
        let registry_title_sync = None;
        let (persistence_info, persistence) = crate::session::persistence::load_light(
                &session_info,
                title_backend,
                self.storage_mode,
                Some(self.auth_manager.clone()),
                Some(self.gateway.clone()),
                registry_title_sync,
            )
            .await
            .map_err(|e| crate::session::persistence::io_error_to_acp(&e))?;
        drop(persistence_timer);
        let crate::session::persistence::PersistedInfoLight {
            summary,
            chat_history,
            plan_state: _,
            plan_mode_state: persisted_plan_mode,
            updates_file_path,
            rewind_points_file_path,
            signals: persisted_signals,
            announcement_state: persisted_announcement_state,
            goal_mode_state: _persisted_goal_mode,
        } = persistence_info;
        let restored_compaction_count = persisted_signals
            .as_ref()
            .map(|s| s.compaction_count as u64)
            .unwrap_or(0);
        let restored_turn_count = persisted_signals
            .as_ref()
            .map(|s| s.turn_count as u64)
            .unwrap_or(0);
        let restored_tool_call_count = persisted_signals
            .as_ref()
            .map(|s| s.tool_call_count as u64)
            .unwrap_or(0);
        let restored_plan_mode_state = match &persisted_plan_mode {
            Some(s) => {
                match s.state {
                    crate::session::plan_mode::PlanModeState::Inactive => {
                        atelier_telemetry::events::PlanModeState::Inactive
                    }
                    crate::session::plan_mode::PlanModeState::Pending => {
                        atelier_telemetry::events::PlanModeState::Pending
                    }
                    crate::session::plan_mode::PlanModeState::Active
                    | crate::session::plan_mode::PlanModeState::ExitPending => {
                        atelier_telemetry::events::PlanModeState::Active
                    }
                }
            }
            None => atelier_telemetry::events::PlanModeState::Inactive,
        };
        let restored_awaiting_plan_approval = persisted_plan_mode
            .as_ref()
            .is_some_and(|s| s.awaiting_plan_approval);
        self.session_turn_numbers
            .borrow_mut()
            .insert(session_id.clone(), summary.next_trace_turn);
        tracing::info!(
            session_id = % session_id.0, next_trace_turn = summary.next_trace_turn,
            "Loaded session telemetry turn counter from persistence"
        );
        let no_replay = parse_no_replay(request_meta.as_ref());
        let cursor = request_meta
            .as_ref()
            .and_then(|m| m.get("cursor"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let session_yolo_mode = request_meta
            .as_ref()
            .and_then(|m| m.get("yoloMode"))
            .and_then(|v| v.as_bool())
            .unwrap_or(self.default_yolo_mode);
        let session_auto_mode = resolve_session_auto_mode(
            request_meta.as_ref(),
            self.default_auto_mode,
            session_yolo_mode,
        );
        #[allow(unused_variables)]
        let session_computer_sessions = parse_session_computer_sessions(
            request_meta.as_ref(),
        );
        let restore_code_requested = request_meta
            .as_ref()
            .and_then(|m| m.get("atelier/restore_code"))
            .and_then(|v| v.as_bool())
            .unwrap_or(self.restore_code);
        let registry_client_for_restore = self.local_session_catalog();
        if restore_code_requested && registry_client_for_restore.is_none() {
            atelier_workspace::session::git::warn_registry_disabled_restore(
                session_id.0.as_ref(),
            );
        }
        let restore_checkout_allowed = atelier_workspace::session::git::restore_code_checkout_allowed(
            cwd.as_path(),
            Some(summary.info.cwd.as_str()),
        );
        if restore_code_requested && !restore_checkout_allowed
            && let Some(ref target_sha) = summary.head_commit
        {
            tracing::warn!(
                target : atelier_workspace::session::git::RESTORE_CODE_LOG, session_id =
                % session_id.0, supplied_cwd = % cwd.as_str(), persisted_cwd = % summary
                .info.cwd, target_sha = % target_sha,
                "restore_code: skipping session HEAD checkout — supplied cwd is neither a atelier worktree nor the session's persisted cwd (refusing to detach the source repo)"
            );
            atelier_telemetry::unified_log::warn(
                "restore_code: skipped session HEAD checkout (unsafe cwd)",
                Some(session_id.0.as_ref()),
                Some(
                    serde_json::json!(
                        { "supplied_cwd" : cwd.as_str(), "persisted_cwd" : summary.info
                        .cwd, "target_sha" : target_sha, }
                    ),
                ),
            );
        }
        let mut code_restore_info: Option<serde_json::Value> = None;
        if restore_code_requested && restore_checkout_allowed
            && let Some(ref target_sha) = summary.head_commit
        {
            use atelier_workspace::session::git::RestoreKind;
            let outcome = atelier_workspace::session::git::checkout_session_commit(
                    cwd.as_path(),
                    target_sha,
                    true,
                    session_id.0.as_ref(),
                )
                .await;
            let kind = if !outcome.checked_out {
                RestoreKind::CheckoutFailed
            } else {
                match registry_client_for_restore {
                        None => RestoreKind::RegistryOff,
                        Some(registry_client) => {
                            let _ = registry_client;
                            RestoreKind::RegistryOff
                        }
                    }
            };
            code_restore_info = crate::agent::restore_code::build_code_restore_meta(
                target_sha,
                &outcome,
                kind,
            );
        }
        let load_envrc = {
            let skip_envrc = request_meta
                .as_ref()
                .and_then(|m| m.get("atelier/skip_envrc"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if skip_envrc {
                false
            } else {
                self.cfg.borrow().session.load_envrc.unwrap_or(true)
            }
        };
        let (initial_total_tokens, delta_completions, unfinished_subagents) = if no_replay {
            tracing::info!(
                session_id = % session_id.0,
                "Skipping session replay (noReplay flag set by relay)"
            );
            (
                Self::extract_initial_tokens_from_updates(&updates_file_path),
                Vec::new(),
                Vec::new(),
            )
        } else {
            let (tokens, replay_end_offset, unfinished_subagents) = self
                .replay_session_updates(
                    &session_id,
                    &cwd,
                    &updates_file_path,
                    persist_data.as_ref(),
                    target_client_id.as_ref(),
                    cursor.as_deref(),
                )
                .await?;
            let cursor_mark_replay = cursor.is_none();
            let _timer = crate::instrumentation_timer!("session.delta_flush_replay");
            let completions = match self.flush_session(&session_id).await {
                Ok(()) => {
                    self.replay_session_updates_from_offset_enqueue(
                        &session_id,
                        &updates_file_path,
                        replay_end_offset,
                        persist_data.as_ref(),
                        target_client_id.as_ref(),
                        cursor_mark_replay,
                    )
                }
                Err(reason) => {
                    tracing::warn!(
                        session_id = % session_id.0, reason,
                        "Post-replay flush failed, skipping delta replay"
                    );
                    Vec::new()
                }
            };
            (tokens, completions, unfinished_subagents)
        };
        if let Some(handle) = self.sessions.borrow().get(&session_id) {
            handle.gateway_enabled.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for rx in delta_completions {
            let _ = rx.await;
        }
        let reconcile_completions = {
            let _timer = crate::instrumentation_timer!("session.reconcile_stale_tasks");
            self.reconcile_stale_background_tasks(&session_id, &updates_file_path)
        };
        for rx in reconcile_completions {
            let _ = rx.await;
        }
        let preloaded_envrc = atelier_workspace::envrc::load_envrc_or_empty_when_trusted(
            cwd.as_path(),
            load_envrc && folder_trust::project_scope_allowed(cwd.as_path()),
        );
        let client_code_nav_enabled = request_meta
            .as_ref()
            .and_then(|m| m.get("codeNavEnabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| self.code_nav_enabled.get());
        let (client_terminal, client_fs_read, client_fs_write) = Self::resolve_client_io_caps(
            request_meta.as_ref(),
            init,
        );
        let prompt_display_cwd = request_meta
            .as_ref()
            .and_then(|m| m.get("atelier/display_cwd"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| summary.prompt_display_cwd.clone());
        if self.sessions.borrow().get(&session_id).is_none() {
            tracing::info!(
                session_id = % session_id.0,
                "load_session: spawning new session actor (session not in memory)"
            );
            let mut spawn_timer = crate::instrumentation_timer!(
                "session.spawn_and_register_session"
            );
            spawn_timer.with_field("session_id", session_id.0.as_ref());
            let persisted_agent_name: Option<String> = summary
                .agent_name
                .clone()
                .or_else(|| {
                    self
                        .resolve_model_id(&summary.current_model_id)
                        .ok()
                        .map(|m| m.info().agent_type.clone())
                });
            self.spawn_and_register_session(
                    init,
                    SessionSpawnOptions {
                        session_info,
                        cwd: cwd.clone(),
                        mcp_servers,
                        initial_client_mcp_servers,
                        mcp_meta_config_map,
                        persistence,
                        chat_history,
                        rewind_points_file_path,
                        initial_total_tokens,
                        origin_client: origin_client.clone(),
                        client_code_nav_enabled,
                        client_terminal,
                        client_fs_read,
                        client_fs_write,
                        preloaded_envrc: Some(preloaded_envrc),
                        persisted_signals,
                        persisted_plan_mode,
                        persisted_goal_mode: _persisted_goal_mode,
                        persisted_announcement_state,
                        session_meta: request_meta.as_ref(),
                        managed_mcp_expires_at,
                        model_agent_type: persisted_agent_name.as_deref(),
                        session_model_id: summary.current_model_id.clone(),
                        session_role: self
                            .configured_role(atelier_provider::RoleId::Main)?
                            .map(|role| (atelier_provider::RoleId::Main, role)),
                        session_yolo_mode,
                        session_auto_mode: session_auto_mode && !session_yolo_mode,
                        prompt_display_cwd,
                    },
                )
                .await?;
            drop(spawn_timer);
        } else if !mcp_servers.is_empty() {
            tracing::info!(
                session_id = % session_id.0, mcp_server_count = mcp_servers.len(),
                "load_session: reconnecting to existing session, updating MCP servers"
            );
            if let Some(handle) = self.sessions.borrow_mut().get_mut(&session_id) {
                handle.initial_client_mcp_servers = initial_client_mcp_servers;
                let (tx, _rx) = tokio::sync::oneshot::channel();
                let _ = handle
                    .cmd_tx
                    .send(crate::session::SessionCommand::UpdateMcpServers {
                        mcp_servers,
                        respond_to: tx,
                    });
            }
        } else {
            tracing::info!(
                session_id = % session_id.0,
                "load_session: reconnecting to existing session (feedback manager already initialized)"
            );
        }
        {
            let init_meta = self
                .initialize_request
                .get()
                .and_then(|init| init.meta.as_ref());
            if let Some(handle) = self.sessions.borrow().get(&session_id) {
                enqueue_replace_system_prompt_override(
                    &handle.cmd_tx,
                    request_meta.as_ref(),
                    init_meta,
                );
            }
        }
        if session_exists
            && let Some(hooks) = crate::extensions::hooks::reconnect_client_hooks(
                request_meta.as_ref(),
            ) && let Some(handle) = self.sessions.borrow().get(&session_id)
        {
            handle.set_client_hooks(hooks);
        }
        #[allow(unused_variables)]
        let local_transcript_rendered = !no_replay
            && updates_file_path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .is_some_and(|m| m.len() > 0);
        if let Some(handle) = self.sessions.borrow_mut().get_mut(&session_id) {
            handle.code_nav_enabled = client_code_nav_enabled;
            if session_yolo_mode && !handle.yolo_mode {
                tracing::debug!(
                    session_id = % session_id.0,
                    "Setting YOLO mode on reconnect from load_session request metadata"
                );
                handle.yolo_mode = true;
                let _ = handle
                    .cmd_tx
                    .send(SessionCommand::SetYoloMode {
                        enabled: true,
                    });
            }
            if session_auto_mode && !session_yolo_mode
                && crate::util::config::auto_permission_mode_enabled_from_disk()
            {
                tracing::debug!(
                    session_id = % session_id.0,
                    "Setting auto mode on reconnect from load_session request metadata"
                );
                handle.yolo_mode = false;
                let _ = handle
                    .cmd_tx
                    .send(SessionCommand::SetAutoMode {
                        enabled: true,
                    });
            }
        }
        self.maybe_spawn_interactive_trust_prompt(
            &session_id,
            cwd.as_path(),
            local_runtime_settings.as_ref(),
        );
        if let Some((parent_cmd_tx, session_cwd)) = self
            .sessions
            .borrow()
            .get(&session_id)
            .map(|h| (h.cmd_tx.clone(), h.info.cwd.clone()))
        {
            let session_dir = crate::session::persistence::session_dir(
                &SessionInfo {
                    id: session_id.clone(),
                    cwd: session_cwd,
                },
            );
            crate::agent::subagent::reconcile_orphaned_subagents(
                &unfinished_subagents,
                &self.subagent_coordinator.borrow(),
                &session_dir,
                session_id.0.as_ref(),
                &self.gateway,
                Some(&parent_cmd_tx),
            );
        }
        let persisted_model = summary.current_model_id.clone();
        let models = self.models_manager.models();
        let available = self.models_manager.available();
        self.model_unavailable_sessions.borrow_mut().remove(session_id.0.as_ref());
        let resolved_catalog_key = resolve_catalog_key(&models, &persisted_model);
        tracing::debug!(
            session_id = % session_id.0, persisted = % persisted_model.0,
            resolved_catalog_key = ? resolved_catalog_key.as_ref().map(| k | k.0
            .as_ref()), available_count = available.len(), contains_persisted = available
            .contains_key(& persisted_model), available_keys = ? available.keys()
            .take(10).collect::< Vec < _ >> (),
            "load_session: restoring persisted model (debug)"
        );
        let is_atelier_build = persisted_model.0.starts_with("atelier-build");
        let same_family_fallback = if is_atelier_build {
            available.keys().find(|id| id.0.starts_with("atelier-build")).cloned()
        } else {
            available.keys().find(|id| !id.0.starts_with("atelier-build")).cloned()
        };
        let selectable_catalog_key = selectable_catalog_key_for_persisted(
            &models,
            &available,
            &persisted_model,
        );
        let model_id = if let Some(catalog_key) = selectable_catalog_key {
            if catalog_key != persisted_model {
                tracing::info!(
                    session_id = % session_id.0, persisted = % persisted_model.0,
                    catalog_key = % catalog_key.0,
                    "load_session: mapped persisted routing slug to catalog key"
                );
                atelier_telemetry::unified_log::info(
                    "load_session: mapped persisted routing slug to catalog key",
                    Some(session_id.0.as_ref()),
                    Some(
                        serde_json::json!(
                            { "persisted_model" : persisted_model.0.as_ref(),
                            "catalog_key" : catalog_key.0.as_ref(), }
                        ),
                    ),
                );
            }
            catalog_key
        } else if available.is_empty() {
            tracing::warn!(
                session_id = % session_id.0, persisted = % persisted_model.0,
                "load_session: model catalog empty at load; keeping persisted model unverified (catalog fetch may still be in flight)"
            );
            atelier_telemetry::unified_log::warn(
                "load_session: model catalog empty, keeping persisted model unverified",
                Some(session_id.0.as_ref()),
                Some(
                    serde_json::json!(
                        { "persisted_model" : persisted_model.0.as_ref(), }
                    ),
                ),
            );
            persisted_model
        } else if let Some(fallback) = same_family_fallback {
            tracing::warn!(
                session_id = % session_id.0, previous = % persisted_model.0, new = %
                fallback.0,
                "Persisted model no longer available, auto-switching within family"
            );
            let reason = format!(
                "Model \"{}\" is no longer available for your account.", persisted_model
                .0,
            );
            self.send_model_auto_switched(
                    &session_id,
                    &persisted_model,
                    &fallback,
                    &reason,
                )
                .await;
            fallback
        } else {
            let fallback = available
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| persisted_model.clone());
            tracing::warn!(
                session_id = % session_id.0, previous = % persisted_model.0, fallback = %
                fallback.0, available_count = available.len(), available_keys = ?
                available.keys().take(10).collect::< Vec < _ >> (),
                "Persisted model no longer available, no same-family fallback — blocking prompts for this session"
            );
            atelier_telemetry::unified_log::warn(
                "load_session: persisted model unavailable, no same-family fallback",
                Some(session_id.0.as_ref()),
                Some(
                    serde_json::json!(
                        { "persisted_model" : persisted_model.0.as_ref(),
                        "fallback_model" : fallback.0.as_ref(), "available_count" :
                        available.len(), }
                    ),
                ),
            );
            let reason = format!(
                "Model \"{}\" is no longer available. Please start a new session.",
                persisted_model.0,
            );
            let empty_id = acp::ModelId::new(String::new());
            self.send_model_auto_switched(
                    &session_id,
                    &persisted_model,
                    &empty_id,
                    &reason,
                )
                .await;
            self.model_unavailable_sessions
                .borrow_mut()
                .insert(session_id.0.to_string(), persisted_model.clone());
            fallback
        };
        tracing::debug!(
            session_id = % session_id.0, final_model_id = % model_id.0,
            "load_session: resolved final model_id for set_session_model"
        );
        {
            let _timer = crate::instrumentation_timer!("session.restore_model");
            let restore_meta = summary
                .reasoning_effort
                .map(|effort| {
                    let mut map = acp::Meta::new();
                    map.insert(
                        REASONING_EFFORT_META_KEY.to_string(),
                        reasoning_effort_meta_value(effort),
                    );
                    map
                });
            let _ = crate::agent::handlers::model_switch::apply(
                    self,
                    acp::SetSessionModelRequest::new(session_id.to_owned(), model_id)
                        .meta(restore_meta),
                )
                .await;
        }
        let mut response_meta_map = serde_json::Map::new();
        response_meta_map.insert("sessionId".to_string(), serde_json::json!(session_id));
        if let Some(persist) = persist_data {
            response_meta_map.insert("atelier/persist".to_string(), persist);
        }
        let session_cwd = self
            .sessions
            .borrow()
            .get(&session_id)
            .map(|h| h.info.cwd.clone());
        let indexed_roots = session_cwd
            .as_deref()
            .map(|c| self.indexed_roots_for(std::path::Path::new(c)))
            .unwrap_or_default();
        response_meta_map
            .insert("codebaseIndexed".to_string(), serde_json::json!(indexed_roots));
        if summary.head_commit.is_some() && let Some(ref cwd) = session_cwd
            && summary
                .git_root_dir
                .as_deref()
                .is_none_or(|root| {
                    atelier_workspace::session::git::find_git_root_from_path(
                            std::path::Path::new(cwd.as_str()),
                        )
                        .ok()
                        .is_some_and(|current_root| {
                            current_root == std::path::Path::new(root)
                        })
                })
        {
            let _timer = crate::instrumentation_timer!("session.git_divergence");
            let cwd_path = std::path::Path::new(cwd.as_str());
            let current_head = atelier_workspace::session::git::git_cli(
                    cwd_path,
                    &["rev-parse", "HEAD"],
                )
                .await
                .ok();
            if let Some(divergence) = atelier_workspace::session::git::detect_head_divergence(
                summary.head_commit.as_deref(),
                summary.head_branch.as_deref(),
                current_head.as_deref(),
            ) {
                response_meta_map
                    .insert("gitDivergence".to_string(), serde_json::json!(divergence));
            }
        }
        if let Some(info) = code_restore_info {
            response_meta_map.insert("codeRestore".to_string(), info);
        }
        if let Some(running_prompt_id) = self
            .sessions
            .borrow()
            .get(&session_id)
            .and_then(|h| h.current_prompt_id.lock().ok().and_then(|g| g.clone()))
        {
            response_meta_map
                .insert(
                    "atelier/runningPromptId".to_string(),
                    serde_json::json!(running_prompt_id),
                );
        }
        let model_state = self.model_state(Some(&session_id));
        let (session_config_value, session_detail_value) = self
            .session_config_meta(
                &session_id,
                session_cwd.clone().unwrap_or_default(),
                summary.display_title_opt(),
                &model_state,
            );
        response_meta_map.insert("atelier/sessionConfig".to_string(), session_config_value);
        response_meta_map.insert("atelier/sessionDetail".to_string(), session_detail_value);
        let response_meta = serde_json::Value::Object(response_meta_map);
        atelier_telemetry::unified_log::info(
            "session loaded",
            Some(session_id.0.as_ref()),
            None,
        );
        let response = acp::LoadSessionResponse::new()
            .models(Some(model_state))
            .meta(response_meta.as_object().cloned());
        if let Some(handle) = self.sessions.borrow().get(&session_id) {
            let _ = handle.cmd_tx.send(SessionCommand::AdvertiseCommands);
            if restored_awaiting_plan_approval {
                let _ = handle.cmd_tx.send(SessionCommand::RestorePlanApproval);
            }
        }
        if self.product_analytics_enabled() {
            log_event(atelier_telemetry::events::SessionLoad {
                session_id: session_id.0.to_string(),
                compaction_count: restored_compaction_count,
                turn_count: restored_turn_count,
                tool_call_count: restored_tool_call_count,
                plan_mode_state: restored_plan_mode_state,
                permission_mode: if session_yolo_mode {
                    atelier_telemetry::enums::PermissionMode::AlwaysApprove
                } else if session_auto_mode
                    && crate::util::config::auto_permission_mode_enabled_from_disk()
                {
                    atelier_telemetry::enums::PermissionMode::Auto
                } else {
                    atelier_telemetry::enums::PermissionMode::Ask
                },
                model_id: summary.current_model_id.0.to_string(),
                restored_from_disk: true,
            });
        }
        Ok(response)
    }
    #[tracing::instrument(
        name = "agent.prompt",
        skip_all,
        fields(
            session_id = %arguments.session_id.0,
            turn_number = tracing::field::Empty,
            uploads_enabled = tracing::field::Empty,
            upload_reason = tracing::field::Empty,
        )
    )]
    #[allow(unused_mut)]
    async fn prompt(
        &self,
        mut arguments: acp::PromptRequest,
    ) -> Result<acp::PromptResponse, acp::Error> {
        use crate::session::plan_mode::PromptMode;
        tracing::debug!(
            target : "sampling_log", session_id = % arguments.session_id.0,
            "Received prompt request"
        );
        atelier_telemetry::unified_log::info(
            "prompt received",
            Some(arguments.session_id.0.as_ref()),
            None,
        );
        let handle = self
            .session_handle_waiting_for_load(&arguments.session_id)
            .await
            .ok_or_else(|| acp::Error::invalid_params().data("unknown session id"))?;
        if handle.model_id.0.trim().is_empty() {
            return Err(acp::Error::invalid_params().data(UNCONFIGURED_MODEL_MESSAGE));
        }
        if self.models_manager.allowlist_excludes_all() {
            self.send_model_auto_switched(
                    &arguments.session_id,
                    &acp::ModelId::new(String::new()),
                    &acp::ModelId::new(String::new()),
                    "None of your models are allowed by allowed_models. \
                 Broaden it or remove it from your config, then restart.",
                )
                .await;
            return Ok(acp::PromptResponse::new(acp::StopReason::EndTurn));
        }
        let latched_model = self
            .model_unavailable_sessions
            .borrow()
            .get(arguments.session_id.0.as_ref())
            .cloned();
        if let Some(unavailable_model) = latched_model {
            let models = self.models_manager.models();
            let available = self.models_manager.available();
            let restore_model_id = selectable_catalog_key_for_persisted(
                    &models,
                    &available,
                    &unavailable_model,
                )
                .unwrap_or(unavailable_model.clone());
            if available.contains_key(&restore_model_id) {
                tracing::info!(
                    session_id = % arguments.session_id.0, model_id = % restore_model_id
                    .0,
                    "prompt: previously-unavailable model is back in the catalog; restoring it and unblocking the session"
                );
                atelier_telemetry::unified_log::info(
                    "prompt: previously-unavailable model recovered, unblocking session",
                    Some(arguments.session_id.0.as_ref()),
                    Some(
                        serde_json::json!({ "model_id" : restore_model_id.0.as_ref(), }),
                    ),
                );
                self.model_unavailable_sessions
                    .borrow_mut()
                    .remove(arguments.session_id.0.as_ref());
                if let Err(e) = crate::agent::handlers::model_switch::apply(
                        self,
                        acp::SetSessionModelRequest::new(
                            arguments.session_id.clone(),
                            restore_model_id.clone(),
                        ),
                    )
                    .await
                {
                    tracing::warn!(
                        session_id = % arguments.session_id.0, model_id = %
                        restore_model_id.0, error = ? e,
                        "prompt: failed to restore previously-unavailable model; continuing with the session's current model"
                    );
                }
            } else {
                tracing::warn!(
                    session_id = % arguments.session_id.0, unavailable_model = %
                    unavailable_model.0, available_count = available.len(),
                    available_keys = ? available.keys().take(10).collect::< Vec < _ >>
                    (),
                    "prompt blocked: session model unavailable since load and still missing from the catalog"
                );
                atelier_telemetry::unified_log::warn(
                    "prompt blocked: model unavailable",
                    Some(arguments.session_id.0.as_ref()),
                    Some(
                        serde_json::json!(
                            { "unavailable_model" : unavailable_model.0.as_ref(),
                            "available_count" : available.len(), }
                        ),
                    ),
                );
                self.send_model_auto_switched(
                        &arguments.session_id,
                        &acp::ModelId::new(String::new()),
                        &acp::ModelId::new(String::new()),
                        "Your previous model is no longer available and could not \
                     be switched to a compatible model. Please start a new session.",
                    )
                    .await;
                return Ok(acp::PromptResponse::new(acp::StopReason::EndTurn));
            }
        }
        let intake_lock = self.prompt_intake_lock(&arguments.session_id);
        let intake_guard = intake_lock.lock().await;
        let meta_prompt_mode = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("mode"))
            .and_then(|v| v.as_str())
            .map(PromptMode::from_meta_str);
        let prompt_mode = if let Some(mode) = meta_prompt_mode {
            mode
        } else {
            let (mode_tx, mode_rx) = oneshot::channel();
            let _ = handle
                .cmd_tx
                .send(crate::session::SessionCommand::GetCurrentPromptMode {
                    responds_to: mode_tx,
                });
            mode_rx.await.unwrap_or_default()
        };
        let turn_started_at = chrono::Utc::now().to_rfc3339();
        let prompt_id = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("promptId"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.remember_retryable_prompt(&prompt_id, arguments.clone());
        let turn_number = self.allocate_turn_number(&arguments.session_id);
        tracing::Span::current().record("turn_number", turn_number);
        tracing::info!("Setting up prompt tracing");
        let trace_context = self.get_trace_context(&handle.info, turn_number).await;
        let (harness_block_for_upload, upload_flush_timeout) = crate::util::config::load_blocking_upload_config_sync();
        let block_for_upload = self.cfg.borrow().mode == config::AgentMode::Headless
            || harness_block_for_upload;
        let (model_tx, model_rx) = oneshot::channel();
        let _ = handle
            .cmd_tx
            .send(crate::session::SessionCommand::GetCurrentModel {
                responds_to: model_tx,
            });
        let model = model_rx
            .await
            .unwrap_or_else(|_| self.sampling_config.borrow().model.clone());
        let mut parsed_prompt_tx: Option<oneshot::Sender<ParsedPromptInfo>> = None;
        let verbatim = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("verbatim"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let send_now = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("sendNow"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if let Some(ctx) = trace_context.clone() {
            let (tx, parsed_prompt_rx) = oneshot::channel::<ParsedPromptInfo>();
            parsed_prompt_tx = Some(tx);
            let auth = self.auth_manager.current();
            let user_id = auth.as_ref().map(|a| a.user_id.clone());
            let team_id = auth.as_ref().and_then(|a| a.team_id.clone());
            let user_email = auth.and_then(|a| a.email);
            let init_meta = self
                .initialize_request
                .get()
                .and_then(|req| req.meta.as_ref());
            let client_source = init_meta
                .and_then(|meta| {
                    meta
                        .get("clientSource")
                        .or_else(|| meta.get("clientType"))
                        .or_else(|| meta.get("clientIdentifier"))
                })
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let client_version = init_meta
                .and_then(|meta| meta.get("clientVersion"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| self.cfg.borrow().client_version.clone());
            let agent_config = self.cfg.borrow().clone();
            let plugin_registry = self.plugin_registry_snapshot();
            let prompt_images: Vec<agent_client_protocol::ImageContent> = arguments
                .prompt
                .iter()
                .filter_map(|block| {
                    if let agent_client_protocol::ContentBlock::Image(img) = block {
                        Some(img.clone())
                    } else {
                        None
                    }
                })
                .collect();
            let mut prompt_metadata = PromptMetadata {
                schema_version: LOCAL_ARTIFACT_SCHEMA_VERSION.to_string(),
                session_id: ctx.session_info.id.0.to_string(),
                turn_number: ctx.turn_number,
                request_id: prompt_id.clone(),
                turn_started_at: turn_started_at.clone(),
                repo_root: None,
                remote_url: None,
                user_id,
                user_email,
                team_id,
                client_source,
                client_version,
                model: model.to_owned(),
                reasoning_effort: ctx
                    .session_handle
                    .reasoning_effort
                    .map(|e| e.as_str().to_string()),
                experiment_id: None,
                host_os: std::env::consts::OS.to_string(),
                host_arch: std::env::consts::ARCH.to_string(),
                prompt_has_image: Some(!prompt_images.is_empty()),
                prompt_was_truncated: Some(false),
                prompt_verbatim: if verbatim { Some(true) } else { None },
                cwd: Some(ctx.session_info.cwd.clone()),
                agent_type: Some(ctx.session_handle.agent_name.clone()),
                shell_version: Some(atelier_version::VERSION.to_string()),
                workspace_type: None,
                sandbox: local_sandbox_telemetry(),
            };
            let (session_copy_tx, session_copy_rx) = oneshot::channel();
            let copy_sent = ctx
                .session_handle
                .cmd_tx
                .send(SessionCommand::CopyFile {
                    respond_to: session_copy_tx,
                })
                .is_ok();
            if !copy_sent {
                tracing::warn!(
                    session_id = % ctx.session_info.id.0, turn_number = ctx.turn_number,
                    "Failed to send CopyFile command, skipping session state upload"
                );
            }
            tokio::spawn({
                let ctx = ctx.clone();
                async move {
                    if let Ok(Ok(info)) = tokio::time::timeout(
                            std::time::Duration::from_secs(120),
                            parsed_prompt_rx,
                        )
                        .await && !info.text.is_empty()
                    {
                        prompt_metadata.prompt_was_truncated = Some(
                            info.full_text.is_some(),
                        );
                        if let Some(full_text) = &info.full_text {
                            write_full_prompt_txt(&ctx, full_text).await;
                        }
                    }
                    write_metadata(&ctx, prompt_metadata).await;
                }
            });
            spawn_artifact_task(
                "before_uploads",
                async move {
                    let before_workspace_fut = async {};
                    futures::join!(
                        write_session_state(&ctx, "before", session_copy_rx),
                        before_workspace_fut, write_config(&ctx,
                        &agent_config),
                        crate::local_artifacts::artifacts::write_config_files(&ctx),
                        write_images(&ctx, &prompt_images), write_plugin_state(&ctx,
                        plugin_registry.as_deref()),
                    );
                },
            );
        }
        let next_trace_turn = self
            .session_turn_numbers
            .borrow()
            .get(&arguments.session_id)
            .copied()
            .unwrap_or_else(|| turn_number.saturating_add(1));
        let _ = handle
            .cmd_tx
            .send(crate::session::SessionCommand::SetNextTraceTurn {
                next_trace_turn,
                request_id: Some(prompt_id.clone()),
            });
        let (tx, mut rx) = oneshot::channel();
        let prompt_client_identifier = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("clientIdentifier"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let prompt_screen_mode = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("screenMode"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let json_schema = arguments
            .meta
            .as_ref()
            .and_then(|m| m.get("outputSchema"))
            .cloned();
        if json_schema.as_ref().is_some_and(|schema| !schema.is_object()) {
            return Err(
                acp::Error::invalid_params()
                    .data("outputSchema must be a JSON object describing a JSON Schema"),
            );
        }
        let turn_id = arguments
            .meta
            .as_ref()
            .and_then(|meta| meta.get("turnId"))
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        let provider_and_model = model.split_once('/');
        let role_id = arguments
            .meta
            .as_ref()
            .and_then(|meta| meta.get("role"))
            .and_then(|value| value.as_str())
            .and_then(|value| value.parse::<atelier_provider::RoleId>().ok())
            .unwrap_or(atelier_provider::RoleId::Main);
        let active_role = self.configured_role(role_id)?;
        self.runtime_begin_request(
            &arguments.session_id,
            &prompt_id,
            turn_id,
            role_id.as_str(),
            provider_and_model.map(|(provider, _)| provider.to_owned()),
            Some(model.clone()),
        );
        self.runtime_set_request_parameters(
            &prompt_id,
            active_role.as_ref().and_then(|role| role.effort.clone()),
            active_role.as_ref().map(|role| role.fast_mode),
        );
        let prompt_tokens = arguments
            .prompt
            .iter()
            .map(|block| match block {
                acp::ContentBlock::Text(text) => atelier_token_estimation::estimate_tokens(&text.text),
                acp::ContentBlock::Image(_) => atelier_token_estimation::estimate_image_tokens(1),
                _ => 0,
            })
            .sum::<u64>();
        let conversation_tokens = handle
            .chat_state_handle
            .get_estimated_total_tokens()
            .await;
        let output_token_budget = handle
            .chat_state_handle
            .get_sampling_config()
            .await
            .and_then(|config| config.max_completion_tokens.map(u64::from));
        let resolved_provider_owned = active_role
            .as_ref()
            .map(|role| role.provider.clone())
            .or_else(|| provider_and_model.map(|(provider, _)| provider.to_owned()));
        let wire_resolution = resolved_provider_owned.as_deref().and_then(|provider| {
            let registry = atelier_provider::ProviderRegistry::load_or_create(
                atelier_config::atelier_home().join("providers.toml"),
            )
            .ok()?;
            resolve_inspector_wire_api(&registry, provider, &model).ok()
        });
        let mut runtime_payload = serde_json::json!({
            "promptBlockCount": arguments.prompt.len(),
            "sendNow": send_now,
            "verbatim": verbatim,
        });
        if let Some(role) = &active_role {
            runtime_payload["role"] = serde_json::json!(role_id.as_str());
            runtime_payload["provider"] = serde_json::json!(role.provider);
            runtime_payload["model"] = serde_json::json!(role.model);
            runtime_payload["payload"] = atelier_acp_runtime::redact_payload(
                &serde_json::Value::Object(role.effective_payload()),
            );
        }
        if let Some(resolved) = &wire_resolution {
            runtime_payload["wireApi"] = serde_json::json!(resolved.wire_api);
            runtime_payload["wireApiSource"] = serde_json::json!(resolved.source);
        }
        self.runtime_set_request_context(
            &prompt_id,
            vec![
                crate::runtime_control::ContextBlock {
                    name: "conversation".to_owned(),
                    source: "session".to_owned(),
                    tokens: conversation_tokens,
                    redacted: false,
                },
                crate::runtime_control::ContextBlock {
                    name: "current_user_input".to_owned(),
                    source: "prompt".to_owned(),
                    tokens: prompt_tokens,
                    redacted: false,
                },
            ],
            conversation_tokens.saturating_add(prompt_tokens),
            output_token_budget,
            runtime_payload,
        );
        self.runtime_set_request_wire_api(
            &prompt_id,
            wire_resolution.as_ref().map(|resolved| {
                serde_json::to_string(&resolved.wire_api)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_owned()
            }),
            wire_resolution.as_ref().map(|resolved| {
                serde_json::to_string(&resolved.source)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_owned()
            }),
        );
        let resolved_provider = resolved_provider_owned.as_deref();
        if let Err(error) = self.enforce_provider_request(
            arguments.session_id.0.as_ref(),
            &prompt_id,
            Some(role_id.as_str()),
            resolved_provider,
        ) {
            self.runtime_finish_request(
                &arguments.session_id,
                atelier_acp_runtime::RuntimeState::Failed,
                Some("policy".to_owned()),
                Some(error.to_string()),
            );
            return Err(error);
        }
        self.runtime_update_status(
            &arguments.session_id,
            atelier_acp_runtime::RuntimeState::WaitingForProvider,
            None,
        );
        let (detach_tx, detach_rx) = tokio::sync::oneshot::channel();
        self.register_detach_waiter(&prompt_id, detach_tx);
        if let Err(error) = handle.cmd_tx.send(SessionCommand::Prompt {
                prompt_id: prompt_id.clone(),
                prompt_blocks: arguments.prompt.clone(),
                prompt_mode,
                client_identifier: prompt_client_identifier,
                screen_mode: prompt_screen_mode,
                verbatim,
                json_schema,
                send_now,
                respond_to: tx,
                persist_ack: None,
                parsed_prompt_tx,
        }) {
            self.clear_detach_waiter(&prompt_id);
            self.runtime_finish_request(
                &arguments.session_id,
                atelier_acp_runtime::RuntimeState::Failed,
                Some("session_dispatch".to_owned()),
                Some(format!("failed to dispatch prompt to session: {error}")),
            );
            return Err(
                acp::Error::internal_error()
                    .data(format!("failed to dispatch prompt to session: {error}")),
            );
        }
        drop(intake_guard);
        self.push_roster_activity_delta(
            &arguments.session_id,
            crate::agent::roster::RosterActivity::Working,
        );
        let stop_result = tokio::select! {
            result = &mut rx => {
                self.clear_detach_waiter(&prompt_id);
                match result {
                    Ok(result) => result,
                    Err(_) => {
                        self.runtime_finish_request(
                            &arguments.session_id,
                            atelier_acp_runtime::RuntimeState::Failed,
                            Some("session_response".to_owned()),
                            Some("session failed to respond".to_owned()),
                        );
                        return Err(acp::Error::internal_error().data("session failed to respond"));
                    }
                }
            }
            _ = detach_rx => {
                self.runtime_mark_task_detached(&prompt_id);
                // The foreground ACP request returns at detach time, but the
                // session actor still owns `rx` and will resolve it when the
                // actual turn finishes. Keep a local observer keyed by the
                // detached request id so a later request in the same session
                // cannot be completed by this older turn's result.
                let agent_ref = crate::agent::mvp_agent::LocalRef::new(self);
                let detached_session_id = arguments.session_id.clone();
                let detached_prompt_id = prompt_id.clone();
                tokio::task::spawn_local(async move {
                    let (runtime_state, diagnostic_message) = match rx.await {
                        Ok(Ok(result)) => (
                            super::runtime::runtime_state_for_stop_reason(&result.stop_reason),
                            matches!(result.stop_reason, acp::StopReason::Cancelled)
                                .then(|| "cancelled by client".to_owned()),
                        ),
                        Ok(Err(_)) | Err(_) => (atelier_acp_runtime::RuntimeState::Failed, None),
                    };
                    agent_ref.get().runtime_finish_request_by_id(
                        &detached_session_id,
                        &detached_prompt_id,
                        runtime_state,
                        (runtime_state == atelier_acp_runtime::RuntimeState::Failed)
                            .then(|| "provider_or_tool".to_owned()),
                        diagnostic_message,
                    );
                    if runtime_state == atelier_acp_runtime::RuntimeState::Completed {
                        agent_ref.get().forget_retryable_prompt(&detached_prompt_id);
                    }
                });
                let mut meta = acp::Meta::new();
                meta.insert("detached".to_owned(), serde_json::Value::Bool(true));
                return Ok(acp::PromptResponse::new(acp::StopReason::EndTurn).meta(meta));
            }
        };
        let runtime_state = match &stop_result {
            Ok(result) => super::runtime::runtime_state_for_stop_reason(&result.stop_reason),
            Err(_) => atelier_acp_runtime::RuntimeState::Failed,
        };
        let cancelled = stop_result
            .as_ref()
            .is_ok_and(|result| matches!(result.stop_reason, acp::StopReason::Cancelled));
        self.runtime_finish_request(
            &arguments.session_id,
            runtime_state,
            stop_result
                .as_ref()
                .err()
                .map(|_| "provider_or_tool".to_owned()),
            stop_result
                .as_ref()
                .err()
                .map(ToString::to_string)
                .or_else(|| cancelled.then(|| "cancelled by client".to_owned())),
        );
        if runtime_state == atelier_acp_runtime::RuntimeState::Completed {
            self.forget_retryable_prompt(&prompt_id);
        }
        let last_turn_usage_for_meta = handle
            .chat_state_handle
            .get_last_turn_usage()
            .await;
        if matches!(
            stop_result, Ok(crate ::session::commands::PromptTurnOk { completion_kind :
            crate ::session::commands::PromptCompletionKind::RemovedFromQueue, .. })
        ) {
            return Ok(
                acp::PromptResponse::new(acp::StopReason::Cancelled)
                    .meta(
                        build_prompt_response_meta(PromptResponseMetaArgs {
                                session_id: &arguments.session_id.to_string(),
                                prompt_id: &prompt_id,
                                total_tokens: 0,
                                model_id: &model,
                                last_turn_usage: None,
                                prompt_usage: None,
                                cancellation_category: None,
                                cancel_trigger: None,
                                structured_output: None,
                            })
                            .as_object()
                            .cloned(),
                    ),
            );
        }
        let cancel_trigger: Option<String> = stop_result
            .as_ref()
            .ok()
            .and_then(|ok| match &ok.completion_kind {
                crate::session::commands::PromptCompletionKind::Cancelled {
                    context: Some(ctx),
                    ..
                } => ctx.trigger.clone(),
                _ => None,
            });
        {
            let mapped = stop_result
                .as_ref()
                .map(|ok| ok.stop_reason)
                .map_err(Clone::clone);
            let (stop_reason_value, agent_result_value) = crate::sampling::error::prompt_complete_fields(
                &mapped,
            );
            let turn_id = arguments
                .meta
                .as_ref()
                .and_then(|m| m.get("turnId"))
                .and_then(|v| v.as_u64());
            let mut payload = serde_json::json!(
                { "sessionId" : arguments.session_id.to_string(), "promptId" : prompt_id
                .as_str(), "stopReason" : stop_reason_value, "agentResult" :
                agent_result_value, }
            );
            if let Some(tid) = turn_id {
                payload["turnId"] = serde_json::json!(tid);
            }
            if let Some(ref t) = cancel_trigger {
                payload["cancelTrigger"] = serde_json::json!(t);
            }
            let params = serde_json::value::to_raw_value(&payload)
                .expect("prompt_complete params serialization");
            self.gateway
                .forward_fire_and_forget(
                    acp::ExtNotification::new(
                        "atelier/session/prompt_complete",
                        params.into(),
                    ),
                );
        }
        {
            let end_activity = if handle
                .pending_interactions
                .lock()
                .map(|g| !g.is_empty())
                .unwrap_or(false)
            {
                crate::agent::roster::RosterActivity::NeedsInput
            } else {
                crate::agent::roster::RosterActivity::Idle
            };
            self.push_roster_activity_delta(&arguments.session_id, end_activity);
        }
        let resolved_model = handle.get_model_metadata().await.resolved_model_id;
        let harness_trace_turns = {
            let (tx, rx) = oneshot::channel();
            if handle
                .cmd_tx
                .send(SessionCommand::TakeHarnessTraceTurns {
                    respond_to: tx,
                })
                .is_ok()
            {
                rx.await.ok().unwrap_or_default()
            } else {
                Vec::new()
            }
        };
        if trace_context.is_some() && !harness_trace_turns.is_empty() {
            self.persist_harness_trace_turns(
                    &arguments.session_id,
                    &handle.info,
                    &handle.cmd_tx,
                    &model,
                    harness_trace_turns,
                )
                .await;
        }
        match stop_result {
            Ok(turn_ok) => {
                let crate::session::commands::PromptTurnOk {
                    stop_reason,
                    total_tokens,
                    turn_snapshot,
                    completion_kind,
                    structured_output,
                    usage: prompt_usage,
                } = turn_ok;
                let subagent_refs = self
                    .subagent_coordinator
                    .borrow()
                    .spawned_refs_for_prompt(&prompt_id);
                let permission_events = self
                    .collect_permission_events(&arguments.session_id);
                let turn_messages: Option<atelier_chat_state::TurnCapture> = {
                    let (tx, rx) = oneshot::channel();
                    if handle
                        .cmd_tx
                        .send(SessionCommand::TakeTurnMessages {
                            respond_to: tx,
                        })
                        .is_ok()
                    {
                        rx.await.ok().flatten()
                    } else {
                        None
                    }
                };
                let streaming_partial = crate::local_artifacts::turn::take_streaming_partial(
                        &handle.cmd_tx,
                        prompt_id.clone(),
                        matches!(stop_reason, acp::StopReason::EndTurn),
                        Some(model.clone()),
                    )
                    .await
                    .map(|mut cap| {
                        cap.reason
                            .get_or_insert_with(|| match &completion_kind {
                                crate::session::commands::PromptCompletionKind::Cancelled {
                                    category,
                                    ..
                                } => {
                                    match category {
                                        Some(cat) => format!("cancelled:{cat:?}"),
                                        None => "cancelled".to_string(),
                                    }
                                }
                                _ => "non_completed".to_string(),
                            });
                        cap
                    });
                let upload_deadline = block_for_upload
                    .then(|| tokio::time::Instant::now() + upload_flush_timeout);
                if let Some(ctx) = trace_context.clone() {
                    let request_id = prompt_id.clone();
                    let (input_tokens, cached_input_tokens, output_tokens) = turn_snapshot
                        .as_ref()
                        .map(|s| (
                            Some(s.turn_input_tokens),
                            Some(s.turn_cached_input_tokens),
                            Some(s.turn_output_tokens),
                        ))
                        .unwrap_or((None, None, None));
                    if let Some(deadline) = upload_deadline {
                        let completed = matches!(stop_reason, acp::StopReason::EndTurn);
                        let start_for_upload = turn_snapshot
                            .as_ref()
                            .and_then(|s| s.start_prompt_mode.clone())
                            .or_else(|| Some(prompt_mode.to_string()));
                        let end_for_upload = turn_snapshot
                            .as_ref()
                            .and_then(|s| s.end_prompt_mode.clone());
                        let result = TurnResultMetadata {
                            schema_version: LOCAL_ARTIFACT_SCHEMA_VERSION,
                            request_id,
                            completed,
                            stop_reason: Some(format!("{stop_reason:?}")),
                            total_tokens: Some(total_tokens),
                            input_tokens,
                            cached_input_tokens,
                            output_tokens,
                            error: None,
                            finished_at: chrono::Utc::now().to_rfc3339(),
                            signals: turn_snapshot.as_ref().map(|s| s.current.clone()),
                            turn_delta: turn_snapshot.as_ref().map(|s| s.delta.clone()),
                            start_prompt_mode: start_for_upload,
                            end_prompt_mode: end_for_upload,
                            resolved_model: resolved_model.clone(),
                            subagents_spawned: subagent_refs.clone(),
                        };
                        write_turn_result(&ctx, &result, ArtifactWriteWait::Deadline { deadline })
                            .await;
                    } else {
                        let snapshot_clone = turn_snapshot.clone();
                        let resolved_model = resolved_model.clone();
                        tokio::spawn(async move {
                            let completed = matches!(
                                stop_reason, acp::StopReason::EndTurn
                            );
                            let start_for_upload = snapshot_clone
                                .as_ref()
                                .and_then(|s| s.start_prompt_mode.clone())
                                .or_else(|| Some(prompt_mode.to_string()));
                            let end_for_upload = snapshot_clone
                                .as_ref()
                                .and_then(|s| s.end_prompt_mode.clone());
                            let result = TurnResultMetadata {
                                schema_version: LOCAL_ARTIFACT_SCHEMA_VERSION,
                                request_id,
                                completed,
                                stop_reason: Some(format!("{stop_reason:?}")),
                                total_tokens: Some(total_tokens),
                                input_tokens,
                                cached_input_tokens,
                                output_tokens,
                                error: None,
                                finished_at: chrono::Utc::now().to_rfc3339(),
                                signals: snapshot_clone.as_ref().map(|s| s.current.clone()),
                                turn_delta: snapshot_clone
                                    .as_ref()
                                    .map(|s| s.delta.clone()),
                                start_prompt_mode: start_for_upload,
                                end_prompt_mode: end_for_upload,
                                resolved_model,
                                subagents_spawned: subagent_refs.clone(),
                            };
                            write_turn_result(&ctx, &result, ArtifactWriteWait::Confirm).await;
                        });
                    }
                }
                if let Some(ctx) = trace_context {
                    let (session_copy_tx, session_copy_rx) = oneshot::channel();
                    let copy_sent = ctx
                        .session_handle
                        .cmd_tx
                        .send(SessionCommand::CopyFile {
                            respond_to: session_copy_tx,
                        })
                        .is_ok();
                    if !copy_sent {
                        tracing::warn!(
                            session_id = % ctx.session_info.id.0, turn_number = ctx
                            .turn_number,
                            "Failed to send CopyFile command, skipping session state upload"
                        );
                    }
                    if turn_number == 0
                        && let Some(client) = self.local_session_catalog()
                    {
                        let cwd_str = handle.info.cwd.clone();
                        let model = self.models_manager.current_model_id().0.to_string();
                        let hostname = gethostname::gethostname()
                            .to_string_lossy()
                            .to_string();
                        let suppress = self
                            .auth_manager
                            .current_or_expired()
                            .is_some_and(|a| a.is_zdr_team());
                        let device_id = if suppress { None } else { Some(agent_id()) };
                        let first_prompt = if suppress {
                            None
                        } else {
                            arguments
                                    .prompt
                                    .iter()
                                    .find_map(|b| {
                                        if let acp::ContentBlock::Text(t) = b {
                                            Some(t.text.clone())
                                        } else {
                                            None
                                        }
                                    })
                        };
                        let sid = arguments.session_id.to_string();
                        tokio::spawn(async move {
                            let git_out = |args: &[&str]| -> Option<String> {
                                atelier_tty_utils::git_command()
                                    .current_dir(&cwd_str)
                                    .args(args)
                                    .output()
                                    .ok()
                                    .filter(|o| o.status.success())
                                    .map(|o| {
                                        String::from_utf8_lossy(&o.stdout).trim().to_string()
                                    })
                                    .filter(|s| !s.is_empty())
                            };
                            let repo_remote_url = git_out(
                                &["remote", "get-url", "origin"],
                            );
                            let repo_branch = git_out(
                                &["rev-parse", "--abbrev-ref", "HEAD"],
                            );
                            let repo_head_at_start = git_out(&["rev-parse", "HEAD"]);
                            let reg_req = crate::agent::local_session_catalog::RegisterRequest {
                                session_id: sid.clone(),
                                cwd: cwd_str,
                                model_id: Some(model),
                                repo_remote_url,
                                repo_branch,
                                repo_head_at_start,
                                hostname: Some(hostname),
                                device_id,
                                parent_session_id: None,
                                session_kind: None,
                                subagent_type: None,
                                subagent_persona: None,
                                subagent_role: None,
                                fork_context_source: None,
                                subagent_depth: None,
                            };
                            if let Err(e) = client.register(&reg_req).await {
                                tracing::warn!(
                                    error = % e, "session registry register failed (non-fatal)"
                                );
                            }
                            let info = crate::session::info::Info {
                                id: agent_client_protocol::SessionId::new(
                                    reg_req.session_id.clone(),
                                ),
                                cwd: reg_req.cwd.clone(),
                            };
                            let summary_path = crate::session::persistence::session_dir(
                                    &info,
                                )
                                .join("summary.json");
                            let summary = if suppress {
                                None
                            } else {
                                std::fs::read(&summary_path)
                                        .ok()
                                        .and_then(|bytes| {
                                            serde_json::from_slice::<
                                                crate::session::persistence::Summary,
                                            >(&bytes)
                                                .ok()
                                        })
                                        .map(|s| s.session_summary)
                                        .filter(|s| !s.is_empty())
                            };
                            if first_prompt.is_some() || summary.is_some() {
                                let upd_req = crate::agent::local_session_catalog::UpdateRequest {
                                    summary,
                                    first_prompt,
                                    last_turn_number: None,
                                    repo_head_at_end: None,
                                    restorable_turn_number: None,
                                };
                                tracing::debug!(
                                    session_id = % reg_req.session_id, has_summary = upd_req
                                    .summary.is_some(), "session registry post-register update"
                                );
                                if let Err(e) = client
                                    .update(&reg_req.session_id, &upd_req)
                                    .await
                                {
                                    tracing::warn!(
                                        error = % e,
                                        "session registry first-prompt update failed (non-fatal)"
                                    );
                                }
                            }
                        });
                    }
                    let registry_turn = i32::try_from(turn_number).unwrap_or(i32::MAX);
                    let cwd_for_git = handle.info.cwd.clone();
                    /// Advances `last_turn_number` immediately after a turn completes.
                    ///
                    /// Fired right after the session turn finishes, before any artifact uploads.
                    /// Sets `last_turn_number` with `repo_head_at_end` and does not wait for
                    /// session-state uploads.
                    async fn advance_last_turn(
                        client: crate::agent::local_session_catalog::LocalSessionCatalog,
                        session_id: String,
                        turn: i32,
                        cwd: String,
                    ) {
                        let repo_head_at_end = atelier_tty_utils::git_command()
                            .current_dir(&cwd)
                            .args(["rev-parse", "HEAD"])
                            .output()
                            .ok()
                            .filter(|o| o.status.success())
                            .map(|o| {
                                String::from_utf8_lossy(&o.stdout).trim().to_string()
                            })
                            .filter(|s| !s.is_empty());
                        let req = crate::agent::local_session_catalog::UpdateRequest {
                            summary: None,
                            first_prompt: None,
                            last_turn_number: Some(turn),
                            repo_head_at_end,
                            restorable_turn_number: None,
                        };
                        if let Err(e) = client.update(&session_id, &req).await {
                            tracing::warn!(
                                error = % e,
                                "session registry last_turn_number update failed (non-fatal)"
                            );
                        }
                    }
                    /// Advances `restorable_turn_number` after required restore artifacts are
                    /// confirmed durable.
                    ///
                    /// Called after the post-turn session archive is confirmed in cloud storage.
                    async fn advance_restorable_turn(
                        client: crate::agent::local_session_catalog::LocalSessionCatalog,
                        session_id: String,
                        turn: i32,
                    ) {
                        let req = crate::agent::local_session_catalog::UpdateRequest {
                            summary: None,
                            first_prompt: None,
                            last_turn_number: None,
                            repo_head_at_end: None,
                            restorable_turn_number: Some(turn),
                        };
                        if let Err(e) = client.update(&session_id, &req).await {
                            tracing::warn!(
                                error = % e,
                                "session registry restorable_turn_number update failed (non-fatal)"
                            );
                        }
                    }
                    if let Some(client) = self.local_session_catalog() {
                        let sid = arguments.session_id.to_string();
                        let cwd = cwd_for_git.clone();
                        tokio::spawn(async move {
                            advance_last_turn(client, sid, registry_turn, cwd).await;
                        });
                    }
                    {
                        let cwd = cwd_for_git.clone();
                        let cmd_tx = handle.cmd_tx.clone();
                        tokio::spawn(async move {
                            let head = atelier_workspace::session::git::get_current_commit(
                                    std::path::Path::new(&cwd),
                                )
                                .await;
                            let branch = atelier_workspace::session::git::get_branch(
                                    std::path::Path::new(&cwd),
                                )
                                .await;
                            let _ = cmd_tx
                                .send(crate::session::SessionCommand::PersistGitHead {
                                    commit: head,
                                    branch,
                                });
                        });
                    }
                    let registry_client_for_restorable = self.local_session_catalog();
                    let registry_sid_for_restorable = arguments.session_id.to_string();
                    let err_ctx = ctx.clone();
                    if let Some(deadline) = upload_deadline {
                        match complete_prompt_trace(
                                ctx,
                                permission_events,
                                session_copy_rx,
                                turn_messages,
                                streaming_partial,
                                ArtifactWriteWait::Deadline { deadline },
                            )
                            .await
                        {
                            Ok(true) => {
                                if let Some(client) = registry_client_for_restorable {
                                    advance_restorable_turn(
                                            client,
                                            registry_sid_for_restorable,
                                            registry_turn,
                                        )
                                        .await;
                                }
                            }
                            Ok(false) => {
                                tracing::debug!(
                                    "session state unconfirmed within the flush budget; \
                                     skipping restorable_turn_number advance"
                                );
                            }
                            Err(e) => {
                                tracing::warn!("Failed to complete prompt trace: {e:?}");
                                crate::local_artifacts::artifacts::flush_then_write_error_manifest(
                                        &err_ctx,
                                        deadline,
                                    )
                                    .await;
                            }
                        }
                    } else {
                        spawn_artifact_task(
                            "after_uploads",
                            async move {
                                match complete_prompt_trace(
                                        ctx,
                                        permission_events,
                                        session_copy_rx,
                                        turn_messages,
                                        streaming_partial,
                                        ArtifactWriteWait::Confirm,
                                    )
                                    .await
                                {
                                    Ok(true) => {
                                        if let Some(client) = registry_client_for_restorable {
                                            advance_restorable_turn(
                                                    client,
                                                    registry_sid_for_restorable,
                                                    registry_turn,
                                                )
                                                .await;
                                        }
                                    }
                                    Ok(false) => {
                                        tracing::warn!(
                                            "Session state upload failed; skipping registry \
                                         restorable_turn_number advance"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!("Failed to complete prompt trace: {e:?}");
                                        write_error_manifest(&err_ctx).await;
                                    }
                                }
                            },
                        );
                    }
                }
                let last_turn_usage = last_turn_usage_for_meta;
                let cancellation_category = match &completion_kind {
                    crate::session::commands::PromptCompletionKind::Cancelled {
                        category: Some(cat),
                        ..
                    } => Some(format!("{cat:?}")),
                    crate::session::commands::PromptCompletionKind::MaxTurnsReached {
                        ..
                    } => Some("max_turns_reached".to_string()),
                    _ => None,
                };
                Ok(
                    acp::PromptResponse::new(stop_reason)
                        .meta(
                            build_prompt_response_meta(PromptResponseMetaArgs {
                                    session_id: &arguments.session_id.to_string(),
                                    prompt_id: &prompt_id,
                                    total_tokens,
                                    model_id: &model,
                                    last_turn_usage: last_turn_usage.as_ref(),
                                    prompt_usage,
                                    cancellation_category,
                                    cancel_trigger,
                                    structured_output,
                                })
                                .as_object()
                                .cloned(),
                        ),
                )
            }
            Err(err) => {
                let subagent_refs = self
                    .subagent_coordinator
                    .borrow()
                    .spawned_refs_for_prompt(&prompt_id);
                let turn_messages: Option<atelier_chat_state::TurnCapture> = {
                    let (tx, rx) = oneshot::channel();
                    if handle
                        .cmd_tx
                        .send(SessionCommand::TakeTurnMessages {
                            respond_to: tx,
                        })
                        .is_ok()
                    {
                        rx.await.ok().flatten()
                    } else {
                        None
                    }
                };
                let err_kind_str = format!("{:?}", err.code);
                let streaming_partial = crate::local_artifacts::turn::take_streaming_partial(
                        &handle.cmd_tx,
                        prompt_id.clone(),
                        false,
                        Some(model.clone()),
                    )
                    .await
                    .map(|mut cap| {
                        cap.reason = Some(format!("sampler_error:{err_kind_str}"));
                        cap
                    });
                if let Some(ctx) = trace_context.clone() {
                    let request_id = prompt_id.clone();
                    let err_str = format!("{err:?}");
                    let stop_reason = crate::sampling::error::stop_reason_for_turn_error(
                            &err,
                        )
                        .to_string();
                    let upload_unified = matches!(
                        crate ::sampling::error::http_status_from_error(& err), Some(401
                        | 404),
                    );
                    let upload_deadline = block_for_upload
                        .then(|| tokio::time::Instant::now() + upload_flush_timeout);
                    if let Some(deadline) = upload_deadline {
                        let result = TurnResultMetadata {
                            schema_version: LOCAL_ARTIFACT_SCHEMA_VERSION,
                            request_id,
                            completed: false,
                            stop_reason: Some(stop_reason),
                            total_tokens: None,
                            input_tokens: None,
                            cached_input_tokens: None,
                            output_tokens: None,
                            error: Some(err_str),
                            finished_at: chrono::Utc::now().to_rfc3339(),
                            signals: None,
                            turn_delta: None,
                            start_prompt_mode: Some(prompt_mode.to_string()),
                            end_prompt_mode: None,
                            resolved_model: resolved_model.clone(),
                            subagents_spawned: subagent_refs.clone(),
                        };
                        let wait = ArtifactWriteWait::Deadline { deadline };
                        write_turn_result(&ctx, &result, wait).await;
                        if let Some(capture) = turn_messages {
                            write_turn_messages(&ctx, capture).await;
                        }
                        if let Some(ref capture) = streaming_partial {
                            crate::local_artifacts::artifacts::write_streaming_partial(
                                    &ctx,
                                    capture,
                                )
                                .await;
                        }
                        if upload_unified {
                            write_unified_log(&ctx, wait).await;
                        }
                        crate::local_artifacts::artifacts::flush_then_write_error_manifest(
                                &ctx,
                                deadline,
                            )
                            .await;
                    } else {
                        let resolved_model = resolved_model.clone();
                        spawn_artifact_task(
                            "error_turn_result",
                            async move {
                                let result = TurnResultMetadata {
                                    schema_version: LOCAL_ARTIFACT_SCHEMA_VERSION,
                                    request_id,
                                    completed: false,
                                    stop_reason: Some(stop_reason),
                                    total_tokens: None,
                                    input_tokens: None,
                                    cached_input_tokens: None,
                                    output_tokens: None,
                                    error: Some(err_str),
                                    finished_at: chrono::Utc::now().to_rfc3339(),
                                    signals: None,
                                    turn_delta: None,
                                    start_prompt_mode: Some(prompt_mode.to_string()),
                                    end_prompt_mode: None,
                                    resolved_model,
                                    subagents_spawned: subagent_refs.clone(),
                                };
                                write_turn_result(&ctx, &result, ArtifactWriteWait::Confirm)
                                    .await;
                                if let Some(capture) = turn_messages {
                                    write_turn_messages(&ctx, capture)
                                        .await;
                                }
                                if let Some(ref capture) = streaming_partial {
                                    crate::local_artifacts::artifacts::write_streaming_partial(
                                            &ctx,
                                            capture,
                                        )
                                        .await;
                                }
                                if upload_unified {
                                    write_unified_log(&ctx, ArtifactWriteWait::Confirm).await;
                                }
                                write_error_manifest(&ctx).await;
                            },
                        );
                    }
                }
                let err = if crate::sampling::error::prompt_usage_from_error(&err)
                    .is_some()
                {
                    err
                } else {
                    let prompt_id = handle
                        .current_prompt_id
                        .lock()
                        .ok()
                        .and_then(|g| g.clone());
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let usage = if handle
                        .cmd_tx
                        .send(crate::session::commands::SessionCommand::ErrorPathUsageFallback {
                            prompt_id,
                            respond_to: tx,
                        })
                        .is_ok()
                    {
                        rx.await.ok().flatten()
                    } else {
                        None
                    };
                    crate::sampling::error::attach_prompt_usage(err, usage)
                };
                Err(err)
            }
        }
    }
    async fn cancel(&self, args: acp::CancelNotification) -> Result<(), acp::Error> {
        tracing::info!("Received cancel request {args:?}");
        let handle = self.session_handle_waiting_for_load(&args.session_id).await;
        let cancel_trigger = args
            .meta
            .as_ref()
            .and_then(|m| m.get("cancelTrigger"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        atelier_telemetry::unified_log::info(
            "shell.cancel.received",
            Some(args.session_id.0.as_ref()),
            Some(
                serde_json::json!(
                    { "session_found" : handle.is_some(), "trigger" : cancel_trigger, }
                ),
            ),
        );
        if let Some(handle) = handle {
            let cancel_subagents = args
                .meta
                .as_ref()
                .and_then(|m| m.get("cancelSubagents"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let rewind_if_pristine = args
                .meta
                .as_ref()
                .and_then(|m| m.get("rewindIfPristine"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let _ = handle
                .cmd_tx
                .send(SessionCommand::Cancel {
                    cancel_subagents,
                    kill_background_tasks: false,
                    rewind_if_pristine,
                    trigger: cancel_trigger,
                });
        }
        Ok(())
    }
    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> Result<acp::SetSessionModeResponse, acp::Error> {
        tracing::info!("Received set session mode request {args:?}");
        let handle = self.session_handle_waiting_for_load(&args.session_id).await;
        let (tx, rx) = oneshot::channel();
        if let Some(handle) = handle {
            let _ = handle
                .cmd_tx
                .send(SessionCommand::SessionMode {
                    session_mode: args.mode_id,
                    responds_to: tx,
                });
        }
        let _ = rx
            .await
            .map_err(|_| {
                acp::Error::internal_error().data("response to set session failed")
            })?;
        Ok(acp::SetSessionModeResponse::new())
    }
    async fn set_session_model(
        &self,
        args: acp::SetSessionModelRequest,
    ) -> Result<acp::SetSessionModelResponse, acp::Error> {
        let model = self.resolve_model_id(&args.model_id)?;
        if !model.info.user_selectable {
            return Err(
                acp::Error::invalid_params()
                    .data("This model isn't allowed by your allowed_models setting."),
            );
        }
        let session_id = args.session_id.clone();
        let res = crate::agent::handlers::model_switch::apply(self, args).await;
        if res.is_ok()
            && let Some(unavailable) = self
                .model_unavailable_sessions
                .borrow_mut()
                .remove(session_id.0.as_ref())
        {
            tracing::info!(
                session_id = % session_id.0, previously_unavailable_model = % unavailable
                .0,
                "set_session_model: user model switch cleared the model-unavailable block"
            );
        }
        res
    }
    #[tracing::instrument(
        name = "agent.ext_method",
        skip_all,
        fields(method = %args.method)
    )]
    async fn ext_method(
        &self,
        args: acp::ExtRequest,
    ) -> Result<acp::ExtResponse, acp::Error> {
        let request_meta = serde_json::from_str::<serde_json::Value>(args.params.get())
            .ok()
            .and_then(|v| v.get("_meta").cloned());
        tracing::info!("Received extension method call: method={}", args.method);
        #[allow(unused_mut)]
        let mut backend_no_bridge_err: Option<acp::Error> = None;
        let method = args.method.clone();
        if is_removed_vendor_extension(method.as_ref()) {
            return Err(acp::Error::method_not_found().data(
                "this vendor-hosted Atelier extension is disabled in the private build",
            ));
        }
        self.enforce_extension_policy(&args)?;
        let result = match method.as_ref() {
            "atelier/getApiKey" | "atelier/setApiKey" => {
                crate::extensions::auth::handle(self, &args).await
            }
            "atelier/session/info" | "atelier/session/close" | "atelier/session/list"
            | "atelier/sessions/list" => {
                crate::agent::handlers::session::handle(self, &args).await
            }
            "atelier/session/updates" => {
                crate::extensions::session_updates::handle(&args, &self.gateway).await
            }
            "atelier/session/load_history" => {
                crate::extensions::chat_conversation_history::handle(self, &args).await
            }
            "atelier/session/search" => {
                crate::extensions::session_search::handle(&args).await
            }
            "atelier/session/resolve_local_for_worktree_resume"
            | "atelier/session/rehydrate" => {
                let ops = self.resolve_workspace_ops()?;
                crate::extensions::worktree::handle(self, &ops, &args).await
            }
            "atelier/session/rename" | "atelier/session/delete"
            | "atelier/session/update_mcp_servers" | "atelier/session/fork"
            | crate::extensions::session_admin::SESSION_FORK
            | "atelier/internal/reload_all_mcp_servers"
            | "atelier/internal/reload_project_mcp_servers" | "atelier/internal/reload_skills"
            | "atelier/internal/reload_models" | "atelier/internal/reload_models_cache"
            | "atelier/internal/auth_cleared" | "atelier/plugins/reload"
            | "atelier/commands/list" => {
                crate::extensions::session_admin::handle(self, &args).await
            }
            "atelier/session/repair" => crate::extensions::repair::handle(self, &args).await,
            "atelier/memory/flush" | "atelier/memory/rewrite" => {
                crate::extensions::memory::handle(self, &args).await
            }
            "atelier/skills/refresh-baseline" => {
                self.refresh_skill_baseline_for_all_sessions();
                crate::extensions::to_ext_response(
                    Ok(serde_json::json!({ "ok" : true })),
                )
            }
            "atelier/interject" => crate::extensions::interject::handle(self, &args).await,
            s if s.starts_with("_atelier/btw/") || s.starts_with("atelier/btw/") => {
                crate::extensions::btw::handle(self, &args).await
            }
            s if s.starts_with("_atelier/context_snapshot/")
                || s.starts_with("atelier/context_snapshot/")
                || s.starts_with("_atelier/agent/spawn_")
                || s.starts_with("atelier/agent/spawn_") => {
                crate::extensions::context_snapshot::handle(self, &args).await
            }
            "atelier/recap" => crate::extensions::recap::handle(self, &args).await,
            "atelier/cloud/terminate" | "atelier/cloud/env/list" => Err(
                acp::Error::method_not_found()
                    .data("vendor cloud sandboxes are not supported by the local runtime"),
            ),
            "atelier/cloud/env/create" => Err(acp::Error::method_not_found()
                .data("vendor cloud sandboxes are not supported by the local runtime")),
            "atelier/cloud/env/update" => Err(acp::Error::method_not_found()
                .data("vendor cloud sandboxes are not supported by the local runtime")),
            "atelier/cloud/env/delete" => Err(acp::Error::method_not_found()
                .data("vendor cloud sandboxes are not supported by the local runtime")),
            "atelier/prompt_history" => {
                crate::extensions::prompt_history::handle(self, &args).await
            }
            "atelier/suggest" => crate::extensions::suggest::handle(self, &args).await,
            "atelier/suggestPrompt" => crate::extensions::suggest::handle(self, &args).await,
            "_atelier/protocol/info" | "atelier/protocol/info" => {
                crate::extensions::runtime::handle(self, &args).await
            }
            s if s.starts_with("_atelier/runtime/")
                || s.starts_with("atelier/runtime/")
                || s.starts_with("_atelier/context/")
                || s.starts_with("atelier/context/")
                || s.starts_with("_atelier/request/")
                || s.starts_with("atelier/request/")
                || s.starts_with("_atelier/trace/")
                || s.starts_with("atelier/trace/")
                || s.starts_with("_atelier/task/") => {
                crate::extensions::runtime::handle(self, &args).await
            }
            s if s.starts_with("_atelier/role/") || s.starts_with("atelier/role/") => {
                crate::extensions::roles::handle(self, &args).await
            }
            s if s.starts_with("_atelier/config/") || s.starts_with("atelier/config/") => {
                crate::extensions::configuration::handle(self, &args).await
            }
            s if s.starts_with("_atelier/policy/") || s.starts_with("atelier/policy/") => {
                crate::extensions::policy::handle(self, &args).await
            }
            s if s.starts_with("atelier/auth/") => {
                crate::extensions::auth::handle(self, &args).await
            }
            s if s.starts_with("_atelier/sandbox/") || s.starts_with("atelier/sandbox/") => {
                crate::extensions::sandbox::handle(self, &args).await
            }
            s if s.starts_with("_atelier/provider/")
                || s.starts_with("_atelier/model/")
                || s.starts_with("_atelier/model_provider_override/")
                || s.starts_with("_atelier/credential/")
                || s.starts_with("atelier/provider/")
                || s.starts_with("atelier/model/")
                || s.starts_with("atelier/model_provider_override/")
                || s.starts_with("atelier/credential/") => {
                crate::extensions::provider::handle(self, &args).await
            }
            s if s.starts_with("atelier/session_summaries/") => {
                crate::agent::handlers::session::handle(self, &args).await
            }
            s if s.starts_with("atelier/git/worktree/") => {
                let ops = self.resolve_workspace_ops()?;
                crate::extensions::worktree::handle(self, &ops, &args).await
            }
            s if s.starts_with("atelier/git/") => {
                let ops = self.resolve_workspace_ops()?;
                crate::extensions::git::handle(self, &ops, &args).await
            }
            s if s.starts_with("atelier/compact_conversation") => {
                crate::extensions::memory::handle(self, &args).await
            }
            s if s.starts_with("atelier/plugins/") => {
                crate::extensions::plugins::handle(self, &args).await
            }
            s if s.starts_with("atelier/marketplace/") => {
                crate::extensions::marketplace::handle(self, &args).await
            }
            s if s.starts_with("atelier/hooks/") => {
                crate::extensions::hooks::handle(self, &args).await
            }
            s if s.starts_with("atelier/hunk-tracker/") => {
                let ops = self.resolve_workspace_ops()?;
                crate::extensions::hunk_tracker::handle(self, &ops, &args).await
            }
            s if s.starts_with("atelier/pr/") => {
                crate::extensions::pr::handle(self, &args).await
            }
            s if s.starts_with(crate::extensions::mcp::mcp_methods::PREFIX) => {
                crate::extensions::mcp::handle(self, &args).await
            }
            s if s.starts_with("atelier/task/") => {
                crate::extensions::task::handle(self, &args).await
            }
            s if s.starts_with("atelier/scheduler/") => {
                crate::extensions::task::handle_scheduler(self, &args).await
            }
            s if s.starts_with("atelier/subagent/") => {
                crate::extensions::task::handle_subagent(self, &args).await
            }
            s if s.starts_with("atelier/terminal/") => {
                crate::extensions::terminal::handle(self, &args).await
            }
            s if crate::extensions::fs::is_fs_method(s) => {
                crate::extensions::fs::handle(self, &args).await
            }
            s if s.starts_with("atelier/search/") => {
                crate::extensions::search::handle(self, &args).await
            }
            s if s.starts_with("atelier/code/") => {
                let ops = self.resolve_workspace_ops()?;
                crate::extensions::code_nav::handle(self, &ops, &args).await
            }
            s if s.starts_with("atelier/skills/") => {
                let compat = self.cfg.borrow().compat_resolved;
                crate::extensions::skills::handle(
                        &args,
                        self.plugin_registry_handle.snapshot().as_deref(),
                        compat,
                    )
                    .await
            }
            s if s.starts_with("atelier/debug/") => {
                crate::extensions::debug::handle(self, &args).await
            }
            s if s.starts_with("atelier/rewind") => {
                crate::extensions::rewind::handle(self, &args).await
            }
            other => {
                Err(
                    acp::Error::method_not_found()
                        .data(format!("unknown ACP extension method: {other}")),
                )
            }
        };
        if let Some(err) = backend_no_bridge_err
            && matches!(
                & result, Err(e) if e.code == acp::Error::method_not_found().code
            )
        {
            return Err(err);
        }
        result
    }
    async fn ext_notification(
        &self,
        args: acp::ExtNotification,
    ) -> Result<(), acp::Error> {
        tracing::info!("Received extension notification: method={}", args.method);
        if args.method.as_ref() == "atelier/yolo_mode_changed"
            && let Ok(params) = serde_json::from_str::<
                serde_json::Value,
            >(args.params.get())
        {
            let sender_id = params.get("clientIdentifier").and_then(|v| v.as_str());
            let permission_mode = params
                .get("permission_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let yolo_signal = params.get("yolo_mode").and_then(|v| v.as_bool());
            if let Some(yolo_mode) = yolo_signal {
                let mut sessions = self.sessions.borrow_mut();
                let updated_sessions = apply_yolo_mode_to_matching_sessions(
                    &mut sessions,
                    sender_id,
                    yolo_mode,
                );
                tracing::info!(
                    yolo_mode, sender = ? sender_id, target_sessions = updated_sessions,
                    total_sessions = sessions.len(),
                    "Setting YOLO mode for matching sessions"
                );
            }
            let auto_mode_explicit = params.get("auto_mode").and_then(|v| v.as_bool());
            let want_auto = auto_mode_explicit == Some(true)
                || permission_mode == "auto";
            let clear_auto = auto_mode_explicit == Some(false)
                || (matches!(permission_mode, "always-approve" | "ask" | "default")
                    && !want_auto);
            let enable_auto = want_auto && yolo_signal != Some(true);
            if enable_auto || clear_auto {
                let enabled = enable_auto;
                let matches_sender = |h: &crate::session::SessionHandle| -> bool {
                    sender_id.is_none()
                        || h.origin_client.as_ref().map(|c| c.product.as_str())
                            == sender_id
                };
                let mut sessions = self.sessions.borrow_mut();
                let total_sessions = sessions.len();
                let mut updated = 0;
                for h in sessions.values_mut() {
                    if !matches_sender(h) {
                        continue;
                    }
                    if h
                        .cmd_tx
                        .send(crate::session::SessionCommand::SetAutoMode {
                            enabled,
                        })
                        .is_ok()
                    {
                        if enabled {
                            h.yolo_mode = false;
                        }
                        updated += 1;
                    }
                }
                tracing::info!(
                    auto_mode = enabled, sender = ? sender_id, target_sessions = updated,
                    total_sessions, "Setting auto permission mode for matching sessions"
                );
            }
        }
        if args.method.as_ref() == "atelier/permissions/reset" {
            let sessions = self.sessions.borrow();
            let updated = sessions
                .values()
                .filter(|h| {
                    h
                        .cmd_tx
                        .send(crate::session::SessionCommand::ResetPermissionState)
                        .is_ok()
                })
                .count();
            tracing::info!(
                target_sessions = updated, total_sessions = sessions.len(),
                "Permission state reset for matching sessions"
            );
        }
        if args.method.as_ref() == "atelier/internal/evict_sessions" {
            self.handle_evict_sessions(&args.params).await;
        }
        if args.method.as_ref() == "atelier/toggle_plan_mode"
            && let Ok(params) = serde_json::from_str::<
                serde_json::Value,
            >(args.params.get())
        {
            let session_id_str = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let handle = self
                .sessions
                .borrow()
                .values()
                .find(|s| s.info.id.0.as_ref() == session_id_str)
                .cloned();
            if let Some(handle) = handle {
                let is_engaged = handle.plan_mode.lock().state()
                    != crate::session::plan_mode::PlanModeState::Inactive;
                let next_mode_id = acp::SessionModeId::new(
                    if is_engaged { "default" } else { "plan" },
                );
                let (tx, rx) = oneshot::channel();
                let _ = handle
                    .cmd_tx
                    .send(SessionCommand::SessionMode {
                        session_mode: next_mode_id.clone(),
                        responds_to: tx,
                    });
                if rx.await.is_err() {
                    tracing::warn!(
                        session_id = % session_id_str, mode_id = % next_mode_id.0,
                        "toggle_plan_mode: session mode update failed"
                    );
                }
            } else {
                tracing::warn!(
                    session_id = % session_id_str, "toggle_plan_mode: session not found"
                );
            }
        }
        if matches!(
            args.method.as_ref(), "atelier/queue/remove" | "atelier/queue/reorder" |
            "atelier/queue/clear" | "atelier/queue/edit" | "atelier/queue/interject"
        )
            && let Ok(params) = serde_json::from_str::<
                serde_json::Value,
            >(args.params.get())
        {
            let session_id_str = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let owner = params
                .get("owner")
                .or_else(|| params.get("clientIdentifier"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let handle = self
                .sessions
                .borrow()
                .values()
                .find(|s| s.info.id.0.as_ref() == session_id_str)
                .cloned();
            if let Some(handle) = handle {
                let cmd = crate::agent::ext_parsers::parse_queue_edit_command(
                    args.method.as_ref(),
                    &params,
                    owner,
                );
                if let Some(cmd) = cmd && handle.cmd_tx.send(cmd).is_err() {
                    tracing::warn!(
                        session_id = % session_id_str, method = % args.method,
                        "queue edit: failed to forward SessionCommand (session actor gone)"
                    );
                }
            } else {
                tracing::warn!(
                    session_id = % session_id_str, method = % args.method,
                    "queue edit: session not found"
                );
            }
        }
        if args.method.as_ref() == "atelier/terminal/pty/input"
            && let Ok(params) = serde_json::from_str::<
                serde_json::Value,
            >(args.params.get())
        {
            crate::extensions::terminal::handle_pty_input(&params).await;
        }
        if args.method.as_ref() == "_atelier/session/update" {
            if let Ok(notification) = serde_json::from_str::<
                SessionNotification,
            >(args.params.get()) {
                tracing::info!(
                    "Storing Atelier extension session notification: session_id={}", notification
                    .session_id.0
                );
                if let Some(handle) = self
                    .sessions
                    .borrow()
                    .get(&notification.session_id)
                {
                    let _ = handle
                        .cmd_tx
                        .send(crate::session::SessionCommand::ExtensionSessionNotification {
                            notification,
                        });
                } else {
                    tracing::warn!(
                        "Received Atelier extension session notification for unknown session: {}",
                        notification.session_id.0
                    );
                }
            } else {
                tracing::warn!("Failed to parse Atelier extension session notification params");
            }
        }
        if args.method.as_ref() == "atelier/telemetry/non_git_decision" {
            #[derive(serde::Deserialize)]
            struct NonGitDecisionParams {
                decision: String,
                session_id: String,
                #[serde(default)]
                client_version: Option<String>,
            }
            if let Ok(params) = serde_json::from_str::<
                NonGitDecisionParams,
            >(args.params.get()) {
                tracing::info!(
                    decision = % params.decision, session_id = % params.session_id,
                    client_version = ? params.client_version, "non_git_decision",
                );
                atelier_telemetry::session_ctx::log_event(atelier_telemetry::events::NonGitDecisionEvent {
                    decision: params.decision,
                    session_id: params.session_id,
                    client_version: params.client_version,
                });
            } else {
                tracing::warn!("Failed to parse non_git_decision telemetry params");
            }
        }
        if args.method.as_ref() == "atelier/telemetry/multi_agent_followup" {
            #[derive(serde::Deserialize)]
            struct MultiAgentFollowupParams {
                preferred_agent_label: char,
                preferred_agent_session_id: Option<String>,
                preferred_agent_model_id: Option<String>,
                /// (label, session_id, model_id)
                other_agents: Vec<(char, Option<String>, Option<String>)>,
            }
            if let Ok(params) = serde_json::from_str::<
                MultiAgentFollowupParams,
            >(args.params.get()) {
                tracing::info!(
                    "Logging multi-agent followup telemetry: preferred_agent={}", params
                    .preferred_agent_label
                );
                let total_agents = 1 + params.other_agents.len();
                atelier_telemetry::session_ctx::log_event(atelier_telemetry::events::MultiAgentFollowup {
                    preferred_agent_label: params.preferred_agent_label.to_string(),
                    preferred_agent_session_id: params.preferred_agent_session_id,
                    preferred_agent_model_id: params.preferred_agent_model_id,
                    other_agents: params
                        .other_agents
                        .into_iter()
                        .map(|(l, s, m)| atelier_telemetry::events::AgentInfo {
                            label: l.to_string(),
                            session_id: s,
                            model_id: m,
                        })
                        .collect(),
                    total_agents,
                });
            } else {
                tracing::warn!("Failed to parse multi-agent followup telemetry params");
            }
        }
        if args.method.as_ref() == "atelier/telemetry/multi_agent_apply" {
            #[derive(serde::Deserialize)]
            struct MultiAgentApplyParams {
                applied_agent_label: char,
                applied_agent_session_id: Option<String>,
                applied_agent_model_id: Option<String>,
                /// (label, session_id, model_id)
                discarded_agents: Vec<(char, Option<String>, Option<String>)>,
            }
            if let Ok(params) = serde_json::from_str::<
                MultiAgentApplyParams,
            >(args.params.get()) {
                tracing::info!(
                    "Logging multi-agent apply telemetry: applied_agent={}", params
                    .applied_agent_label
                );
                let total_agents = 1 + params.discarded_agents.len();
                atelier_telemetry::session_ctx::log_event(atelier_telemetry::events::MultiAgentApply {
                    applied_agent_label: params.applied_agent_label.to_string(),
                    applied_agent_session_id: params.applied_agent_session_id,
                    applied_agent_model_id: params.applied_agent_model_id,
                    discarded_agents: params
                        .discarded_agents
                        .into_iter()
                        .map(|(l, s, m)| atelier_telemetry::events::AgentInfo {
                            label: l.to_string(),
                            session_id: s,
                            model_id: m,
                        })
                        .collect(),
                    total_agents,
                });
            } else {
                tracing::warn!("Failed to parse multi-agent apply telemetry params");
            }
        }
        if args.method.as_ref() == "atelier/telemetry/multi_agent_discard" {
            #[derive(serde::Deserialize)]
            struct MultiAgentDiscardParams {
                /// (label, session_id, model_id)
                discarded_agents: Vec<(char, Option<String>, Option<String>)>,
            }
            if let Ok(params) = serde_json::from_str::<
                MultiAgentDiscardParams,
            >(args.params.get()) {
                tracing::info!(
                    "Logging multi-agent discard telemetry: {} agents discarded", params
                    .discarded_agents.len()
                );
                let total = params.discarded_agents.len();
                atelier_telemetry::session_ctx::log_event(atelier_telemetry::events::MultiAgentDiscard {
                    discarded_agents: params
                        .discarded_agents
                        .into_iter()
                        .map(|(l, s, m)| atelier_telemetry::events::AgentInfo {
                            label: l.to_string(),
                            session_id: s,
                            model_id: m,
                        })
                        .collect(),
                    total_agents_discarded: total,
                });
            } else {
                tracing::warn!("Failed to parse multi-agent discard telemetry params");
            }
        }
        if args.method.as_ref() == atelier_telemetry::unified_log::LOG_METHOD
            && let Ok(params) = serde_json::from_str::<
                atelier_telemetry::unified_log::LogNotificationParams,
            >(args.params.get())
        {
            atelier_telemetry::unified_log::ingest_client_entries(
                params.src,
                &params.entries,
            );
        }
        Ok(())
    }
}

fn role_for_new_session(
    meta: Option<&acp::Meta>,
    is_chat_kind: bool,
    mut load_role: impl FnMut(
        atelier_provider::RoleId,
    ) -> Result<atelier_provider::RoleConfig, acp::Error>,
) -> Result<Option<(atelier_provider::RoleId, atelier_provider::RoleConfig)>, acp::Error> {
    if is_chat_kind {
        return Ok(None);
    }
    let Some(role_value) = meta
        .and_then(|meta| meta.get("atelier/role").or_else(|| meta.get("role")))
        .and_then(|value| value.as_str())
    else {
        return Ok(None);
    };
    let role_id = role_value
        .parse::<atelier_provider::RoleId>()
        .map_err(|error| acp::Error::invalid_params().data(error.to_string()))?;
    if matches!(
        role_id,
        atelier_provider::RoleId::Compact
            | atelier_provider::RoleId::Summary
            | atelier_provider::RoleId::Title
            | atelier_provider::RoleId::Planner
            | atelier_provider::RoleId::Strategist
            | atelier_provider::RoleId::Skeptic
    ) {
        return Err(acp::Error::invalid_params().data(format!(
            "role {role_id} is an internal runtime role and cannot own a user session"
        )));
    }
    if let Some(snapshot) = meta.and_then(|meta| meta.get("atelier/roleSnapshot")) {
        let role = serde_json::from_value::<atelier_provider::RoleConfig>(snapshot.clone())
            .map_err(|error| {
                acp::Error::invalid_params().data(format!("invalid {role_id} role snapshot: {error}"))
            })?;
        if !role.is_configured() {
            return Err(acp::Error::invalid_params()
                .data(format!("role {role_id} snapshot is not configured")));
        }
        return Ok(Some((role_id, role)));
    }
    Ok(Some((role_id, load_role(role_id)?)))
}

const UNCONFIGURED_MODEL_MESSAGE: &str =
    "no model configured; configure a Provider and select a model with /model before sending a prompt";

fn configured_model_unavailable_message(model: &str) -> String {
    let provider = model
        .split_once('/')
        .map(|(provider, _)| provider)
        .unwrap_or(model);
    format!(
        "configured new-session model is unavailable: {model}; configure and enable Provider '{provider}' or update config.toml:model"
    )
}

fn title_backend_for_session(
    result: Result<(crate::sampling::Client, String), acp::Error>,
) -> crate::session::summary::TitleBackend {
    match result {
        Ok((sampling_client, model)) => {
            crate::session::summary::TitleBackend::Enabled {
                sampling_client,
                model,
            }
        }
        Err(error) => {
            let reason = atelier_acp_runtime::redact_text(&error.to_string());
            tracing::warn!(
                target: "atelier.runtime",
                role = "title",
                state = "disabled",
                error = %reason,
                "Title generation disabled; main session startup continues"
            );
            crate::session::summary::TitleBackend::Disabled { reason }
        }
    }
}

fn inspector_model_key(
    provider: &str,
    model: &str,
) -> Result<atelier_provider::ModelKey, atelier_provider::ProviderError> {
    if let Ok(key) = atelier_provider::ModelKey::parse(model)
        && key.provider_id == provider
    {
        return Ok(key);
    }
    atelier_provider::ModelKey::new(provider, model)
}

fn resolve_inspector_wire_api(
    registry: &atelier_provider::ProviderRegistry,
    provider: &str,
    model: &str,
) -> Result<atelier_provider::ResolvedWireApi, atelier_provider::ProviderError> {
    registry.resolve_wire_api(&inspector_model_key(provider, model)?)
}

fn is_removed_vendor_extension(method: &str) -> bool {
    matches!(
        method,
        "atelier/billing"
            | "atelier/auto-topup-rule"
            | "atelier/share_session"
            | "atelier/privacy/setCodingDataRetention"
            | "atelier/rollout/survey"
            | "atelier/feedback"
            | "atelier/feedback/dismiss"
            | "atelier/btw"
            | "atelier/workspaces/list"
    ) || method.starts_with("atelier/cloud/")
        || method.starts_with("atelier/review")
}

fn vendorless_auth_method_allowed(method_id: &str) -> bool {
    method_id == crate::agent::auth_method::LOCAL_PROVIDER_AUTH_METHOD_ID
}

#[cfg(test)]
mod vendorless_extension_tests {
    use crate::agent::auth_method;

    use super::{
        UNCONFIGURED_MODEL_MESSAGE, configured_model_unavailable_message, inspector_model_key,
        is_removed_vendor_extension, resolve_inspector_wire_api, role_for_new_session,
        title_backend_for_session, vendorless_auth_method_allowed,
    };

    #[test]
    fn derived_session_uses_the_requested_role_snapshot_instead_of_main() {
        let mut meta = agent_client_protocol::Meta::new();
        meta.insert("role".into(), serde_json::json!("explore"));
        let mut explore = atelier_provider::RoleConfig::new("example", "explore-model").unwrap();
        explore.effort = Some("low".into());
        explore.fast_mode = true;
        explore.payload.insert("temperature".into(), serde_json::json!(0.1));
        meta.insert(
            "atelier/roleSnapshot".into(),
            serde_json::to_value(&explore).unwrap(),
        );

        let resolved = role_for_new_session(Some(&meta), false, |_| {
            panic!("an embedded derived Role snapshot must not be replaced from the registry")
        })
        .unwrap()
        .unwrap();

        assert_eq!(resolved.0, atelier_provider::RoleId::Explore);
        assert_eq!(resolved.1, explore);
    }

    #[test]
    fn ordinary_new_session_has_no_main_role_fallback() {
        let resolved = role_for_new_session(None, false, |_| {
            panic!("ordinary new Sessions must not load roles.main")
        })
        .unwrap();

        assert!(resolved.is_none());
    }

    #[test]
    fn first_prompt_without_a_selected_model_explains_setup() {
        assert!(UNCONFIGURED_MODEL_MESSAGE.contains("configure a Provider"));
        assert!(UNCONFIGURED_MODEL_MESSAGE.contains("/model"));
    }

    #[test]
    fn unavailable_config_model_error_names_provider_and_remediation() {
        assert_eq!(
            configured_model_unavailable_message("example/deepseek-v4-flash"),
            "configured new-session model is unavailable: example/deepseek-v4-flash; configure and enable Provider 'example' or update config.toml:model"
        );
    }

    #[test]
    fn title_role_failure_disables_only_title_generation() {
        let backend = title_backend_for_session(Err(
            agent_client_protocol::Error::invalid_params()
                .data("role title is not configured"),
        ));

        assert!(matches!(
            backend,
            crate::session::summary::TitleBackend::Disabled { reason }
                if reason.contains("role title is not configured")
        ));
    }

    #[test]
    fn inspector_does_not_prefix_an_already_composite_model_key() {
        let key = inspector_model_key("example", "example/deepseek-v4-flash").unwrap();

        assert_eq!(key.provider_id, "example");
        assert_eq!(key.model_id, "deepseek-v4-flash");
        assert_eq!(key.to_string(), "example/deepseek-v4-flash");
    }

    #[test]
    fn inspector_uses_exact_model_wire_api_resolution() {
        let mut registry = atelier_provider::ProviderRegistry::in_memory();
        registry
            .upsert_provider(atelier_provider::ProviderConfig {
                id: "example".into(),
                display_name: "Example".into(),
                auth: atelier_provider::ProviderAuth::Bearer,
                base_url: url::Url::parse("http://127.0.0.1:4317/v1").unwrap(),
                credential: atelier_provider::CredentialRef::None,
                discovery: atelier_provider::ProviderDiscovery::Static,
                extra_headers: Default::default(),
                enabled: true,
            })
            .unwrap();
        registry
            .upsert_model(atelier_provider::ModelDescriptor {
                key: atelier_provider::ModelKey::new("example", "deepseek-v4-flash").unwrap(),
                display_name: "deepseek-v4-flash".into(),
                description: None,
                wire_api: Some(atelier_provider::WireApi::Responses),
                context_window: Some(128_000),
                capabilities: Default::default(),
                reasoning_efforts: Vec::new(),
                default_effort: None,
                fast_mode: false,
                source: atelier_provider::ModelSource::Static,
                enabled: true,
            })
            .unwrap();

        let resolved =
            resolve_inspector_wire_api(&registry, "example", "example/deepseek-v4-flash").unwrap();

        assert_eq!(resolved.provider, "example");
        assert_eq!(resolved.model, "deepseek-v4-flash");
        assert_eq!(resolved.wire_api, atelier_provider::WireApi::Responses);
        assert_eq!(
            resolved.source,
            atelier_provider::WireApiSource::ModelDefinition
        );
    }

    #[test]
    fn vendorless_authentication_accepts_only_local_provider() {
        assert!(vendorless_auth_method_allowed(
            auth_method::LOCAL_PROVIDER_AUTH_METHOD_ID
        ));
        for method in [
            auth_method::PROVIDER_API_KEY_METHOD_ID,
            auth_method::CACHED_TOKEN_AUTH_METHOD_ID,
            auth_method::ATELIER_COM_METHOD_ID,
            auth_method::OIDC_METHOD_ID,
            "atelier.invalid",
        ] {
            assert!(
                !vendorless_auth_method_allowed(method),
                "vendorless runtime must reject direct auth method {method}"
            );
        }
    }

    #[test]
    fn vendor_hosted_extensions_are_blocked() {
        for method in [
            "atelier/cloud/env/list",
            "atelier/feedback",
            "atelier/privacy/setCodingDataRetention",
            "atelier/review/submit",
            "atelier/workspaces/list",
        ] {
            assert!(is_removed_vendor_extension(method), "{method}");
        }
        assert!(!is_removed_vendor_extension("atelier/provider/list"));
        assert!(!is_removed_vendor_extension("atelier/session/list"));
    }
}
