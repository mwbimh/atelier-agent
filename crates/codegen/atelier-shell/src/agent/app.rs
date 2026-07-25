use parking_lot::Mutex;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use agent_client_protocol as acp;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, simplex};
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::time::Duration;
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};
use tracing::{debug, info, warn};

use atelier_acp_runtime::{
    AcpAgentGatewayReceiver as GatewayReceiver, AcpAgentGatewaySender as GatewaySender,
    LineBufferedRead,
};

use crate::agent::config::{Config as AgentConfig, ModelEntry};
use crate::agent::init::{bootstrap, exit_on_config_error};
use crate::agent::models::{ModelFetchAuth, prefetch_models_blocking};
use crate::agent::mvp_agent::MvpAgent;
use crate::auth::{AtelierAuth, AuthManager, AuthMode, run_auth_flow};
use crate::util::atelier_home;
use dirs;

const MAX_BUFFER_SIZE: usize = 8 * 1024 * 1024;

use indexmap::IndexMap;

/// Prefetch models from the API (must be called outside LocalSet).
async fn prefetch_models(agent_config: &AgentConfig) -> Option<IndexMap<String, ModelEntry>> {
    let _ = agent_config;
    // Atelier model discovery is owned by the local Provider registry. The
    // inherited remote `/models` prefetch would both bypass Provider CRUD and
    // reintroduce a hidden network path during startup.
    None
}

/// Spawn the agent inside a LocalSet and return a handle to the I/O future.
fn spawn_agent_local(
    agent_config: AgentConfig,
    auth_manager: Arc<AuthManager>,
    prefetched_models: Option<IndexMap<String, ModelEntry>>,
    memory_config: Option<crate::config::MemoryConfig>,
    outgoing: impl futures::AsyncWrite + Unpin + 'static,
    incoming: impl futures::AsyncRead + Unpin + 'static,
) -> impl std::future::Future<Output = Result<(), acp::Error>> {
    let (gw_tx, gw_rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(gw_tx);
    let mut agent = MvpAgent::new(gateway, &agent_config, auth_manager, prefetched_models)
        .unwrap_or_else(exit_on_config_error);
    if let Some(mc) = memory_config {
        agent.set_memory_config(mc);
    }
    let incoming = LineBufferedRead::spawn_local(incoming);
    let (conn, handle_io) = acp::AgentSideConnection::new(agent, outgoing, incoming, |fut| {
        tokio::task::spawn_local(fut);
    });
    tokio::task::spawn_local(GatewayReceiver::new(gw_rx, conn).run());
    handle_io
}

/// Build a newline-terminated JSON-RPC request line for an internal
/// `atelier/...` extension method, for injection into the agent's inbound ACP
/// stream by the leader's own watcher tasks (config hot-reload, skills).
///
/// The wire method is written **`_`-prefixed** (`_atelier/internal/...`):
/// `agent-client-protocol`'s inbound decoder routes a non-built-in method to
/// `ext_method` only when it carries the `_` extension prefix and rejects
/// bare custom methods with `-32601 method_not_found`. These injections were
/// historically sent un-prefixed, so every watcher-driven hot-reload
/// (models, skills, MCP servers) was silently rejected at decode — the
/// watcher-side "change detected" logs fired but the reload handlers never
/// ran. Keep `method` here as the un-prefixed name; the prefix is a wire
/// detail added in one place.
fn internal_reload_request_line(id: &str, method: &str, params: serde_json::Value) -> String {
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": format!("_{method}"),
        "params": params,
    });
    format!("{}\n", msg)
}

/// Start a skills file watcher and wire it to inject `atelier/internal/reload_skills`
/// messages into the shared ACP incoming stream when SKILL.md files change on disk.
///
/// Returns the watcher guard (must be kept alive for the lifetime of the session)
/// or `None` if no directories could be watched.
fn spawn_skills_file_watcher<W>(
    acp_incoming_tx: &Arc<TokioMutex<W>>,
    skills_paths: &[String],
) -> Option<crate::config::watcher::SkillsFileWatcher>
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let cwd = std::env::current_dir().unwrap_or_default();
    let workspace_user_dir = atelier_agent::prompt::workspace_user::optional_workspace_user_dir();
    let (watcher, mut skills_rx) = crate::config::watcher::SkillsFileWatcher::start(
        Some(cwd.as_path()),
        workspace_user_dir.as_deref(),
        skills_paths,
    )?;
    let skills_tx = acp_incoming_tx.clone();
    tokio::spawn(async move {
        while skills_rx.recv().await.is_some() {
            info!("Skill directory changed on disk, reloading skills for all sessions");
            let line = internal_reload_request_line(
                "skills-reload",
                "atelier/internal/reload_skills",
                serde_json::json!({}),
            );
            let mut tx = skills_tx.lock().await;
            if let Err(e) = tx.write_all(line.as_bytes()).await {
                warn!(
                    error = %e,
                    "failed to inject skills reload into ACP stream"
                );
            }
        }
    });
    Some(watcher)
}

/// Register the process-lifetime runtime so shared filesystem watchers
/// ([`atelier_fsnotify::shared`]) run their event loops on a runtime that outlives
/// individual sessions (each session builds its own short-lived runtime).
/// Idempotent — safe to call from every agent entrypoint.
fn register_fs_watch_runtime() {
    atelier_fsnotify::set_runtime_handle(tokio::runtime::Handle::current());
}

pub async fn run_stdio_agent(
    agent_config: &AgentConfig,
    prefetched_models: Option<IndexMap<String, ModelEntry>>,
    memory_config: Option<crate::config::MemoryConfig>,
) -> anyhow::Result<()> {
    register_fs_watch_runtime();
    // Stamp binary version into unified log entries so zombie processes
    // are identifiable by version in diagnostic logs.
    atelier_telemetry::unified_log::set_version(atelier_version::VERSION);

    // Log the client that launched us (set by atelier-desktop when spawning `atelier agent stdio`).
    // This appears early in unified.jsonl and is extremely useful for auth diagnostics.
    if let Ok(version) = std::env::var("ATELIER_CLIENT_VERSION") {
        crate::unified_log::info(
            "ATELIER_CLIENT_VERSION",
            None,
            Some(serde_json::json!({ "version": version })),
        );
    }

    let _total_timer = crate::instrumentation_timer!("startup.stdio_agent_total");
    let outgoing = tokio::io::stdout().compat_write();
    let prefetched_models = if prefetched_models.is_some() {
        prefetched_models
    } else {
        let _timer = crate::instrumentation_timer!("startup.stdio_prefetch_models");
        prefetch_models(agent_config).await
    };
    let agent_config = agent_config.clone();

    // Use a simplex intermediary between stdin and the agent so we can
    // inject internal messages (e.g. skill-reload) alongside real client
    // input. This mirrors the pattern used by `run_leader`.
    let (acp_incoming_rx, acp_incoming_tx) = simplex(MAX_BUFFER_SIZE);
    let incoming = acp_incoming_rx.compat();
    let acp_incoming_tx = Arc::new(TokioMutex::new(acp_incoming_tx));

    // Bridge stdin to the simplex writer. A dedicated OS thread does the
    // blocking stdin reads (see `atelier_acp_runtime::spawn_stdin_line_reader`): on
    // Windows `tokio::io::stdin()` only delivers buffered lines from a
    // redirected pipe at EOF, so a persistent ACP client (which keeps stdin
    // open) would hang the `initialize` handshake. The forwarder writes each
    // complete line to the simplex so injected internal messages (from the
    // skills watcher) never interleave mid-line with client data.
    let stdin_tx = acp_incoming_tx.clone();
    let (stdin_closed_tx, stdin_closed_rx) = tokio::sync::oneshot::channel();
    let mut stdin_lines = atelier_acp_runtime::spawn_stdin_line_reader();
    tokio::spawn(async move {
        while let Some(line) = stdin_lines.recv().await {
            let mut tx = stdin_tx.lock().await;
            if tx.write_all(&line).await.is_err() {
                break;
            }
        }
        // Signal that stdin closed. The actual simplex shutdown is performed
        // on the LocalSet so pending ACP request handlers can flush their
        // responses first (they run on the same LocalSet and would be
        // starved by an immediate cross-thread shutdown).
        let _ = stdin_closed_tx.send(());
    });

    let _skills_watcher = spawn_skills_file_watcher(&acp_incoming_tx, &agent_config.skills.paths);

    let local_set = tokio::task::LocalSet::new();
    let result = local_set
        .run_until(async move {
            // Shut down the simplex writer on the LocalSet so it's cooperative with ACP handlers.
            let simplex_tx = acp_incoming_tx;
            tokio::task::spawn_local(async move {
                let _ = stdin_closed_rx.await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                let mut tx = simplex_tx.lock().await;
                let _ = tx.shutdown().await;
            });

            // Create the auth manager here (not in `spawn_agent_local`) so the session-start refresh can
            // drive a token refresh before bootstrap reads policy; the same manager goes to the agent.
            let auth_manager = Arc::new(agent_config.create_auth_manager());
            // Proactive token refresh; runs until process exit.
            auth_manager.start_proactive_refresh(tokio_util::sync::CancellationToken::new());
            // Pause refreshes across system sleep so an OIDC refresh can't straddle a
            // suspend (which can revoke the refresh token and force re-login).
            // `atelier agent stdio` is a local/interactive entrypoint (spawned by
            // atelier-desktop), so it needs the gate like the leader and pager paths;
            // no-op where the OS listener is unavailable.
            auth_manager.start_system_power_listener();

            // Restore managed policy right before bootstrap reads it (no stale window after prefetch).
            let handle_io = spawn_agent_local(
                agent_config,
                auth_manager,
                prefetched_models,
                memory_config,
                outgoing,
                incoming,
            );
            handle_io.await?;
            Ok::<(), anyhow::Error>(())
        })
        .await;
    // Kill PTY child processes so they don't outlive the agent.
    crate::terminal::pty_session::close_all().await;

    // Brief grace period for the upload queue worker to finish in-flight uploads.
    // The worker runs on the tokio runtime (not the LocalSet), so it continues
    // after the LocalSet drops. The channel closes when all senders drop (agent
    // exit), and the worker drains remaining items before exiting.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    result
}

pub async fn run_headless(
    _agent_config: &AgentConfig,
    _reauthenticate: bool,
    _memory_config: Option<crate::config::MemoryConfig>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "Atelier has no vendor relay mode; use atelier agent stdio or atelier agent serve"
    )
}

/// Browserless vendor relay mode was removed with the vendor service integration.
pub async fn run_headless_no_browser(
    _agent_config: &AgentConfig,
    _memory_config: Option<crate::config::MemoryConfig>,
) -> anyhow::Result<()> {
    anyhow::bail!(
        "Atelier has no vendor relay mode; use atelier agent stdio or atelier agent serve"
    )
}
/// Migrate a legacy devbox WebLogin token to fresh OIDC in place (mint, persist,
/// drop the legacy scope). No-op outside a devbox or for non-WebLogin / `None`.
/// On mint/save failure, returns the existing token so the leader still starts.
async fn migrate_devbox_auth_if_legacy(
    auth: Option<AtelierAuth>,
    agent_config: &AgentConfig,
) -> Option<AtelierAuth> {
    let auth = auth?;
    if !crate::auth::devbox_login::is_devbox_environment() || auth.auth_mode != AuthMode::WebLogin {
        return Some(auth);
    }

    info!("Devbox legacy auth detected, attempting migration to OIDC");
    atelier_telemetry::unified_log::info(
        "devbox legacy auth migration: starting",
        None,
        Some(serde_json::json!({
            "user_id": auth.user_id,
            "auth_mode": format!("{:?}", auth.auth_mode),
        })),
    );

    // save + remove_scope are two non-atomic writes to auth.json (no lock). Safe
    // at startup: no concurrent writer yet, and `lookup_auth` prefers the primary
    // scope if a reader sees the intermediate state.
    let migration_auth_manager = agent_config.create_auth_manager();

    let new_auth = match crate::auth::devbox_login::mint_devbox_auth(&migration_auth_manager).await
    {
        Ok(new_auth) => new_auth,
        Err(e) => {
            tracing::warn!(error = ?e, "devbox legacy auth migration: devbox login helper call failed, continuing with legacy auth");
            atelier_telemetry::unified_log::error(
                "devbox legacy auth migration: mint failed",
                None,
                Some(serde_json::json!({ "error": e.to_string() })),
            );
            return Some(auth);
        }
    };
    match migration_auth_manager
        .save_without_enrichment(new_auth)
        .await
    {
        Ok(saved_auth) => {
            if let Err(e) = migration_auth_manager.remove_scope(crate::auth::LEGACY_AUTH_SCOPE) {
                tracing::warn!(error = ?e, "Failed to remove legacy auth scope entry (non-fatal)");
            }
            atelier_telemetry::unified_log::info(
                "devbox legacy auth migration: succeeded",
                None,
                Some(serde_json::json!({
                    "user_id": saved_auth.user_id,
                    "has_refresh_token": saved_auth.refresh_token.is_some(),
                    "expires_at": saved_auth.expires_at.map(|e| e.to_rfc3339()),
                    "auth_mode": format!("{:?}", saved_auth.auth_mode),
                })),
            );
            info!(user_id = %saved_auth.user_id, "Devbox legacy auth migrated to OIDC successfully");
            Some(saved_auth)
        }
        Err(e) => {
            tracing::warn!(error = ?e, "devbox legacy auth migration: failed to save new auth, continuing with legacy");
            atelier_telemetry::unified_log::error(
                "devbox legacy auth migration: save failed",
                None,
                Some(serde_json::json!({ "error": e.to_string() })),
            );
            Some(auth)
        }
    }
}

/// Run the agent in leader mode, accepting IPC connections from multiple clients.
/// When a atelier.invalid session is present, the leader connects to the websocket relay
/// after startup (post-auth, post-prefetch); BYOK / no-session leaders skip it and
/// serve clients over IPC only. See [`spawn_leader_relay`] for when the relay
/// connection is opened (eager by default, demand-gated with `relay_on_demand`).
///
/// Startup sequence:
/// 1. Lock acquisition check — bail if another leader is already running.
/// 2. Socket cleanup, channel + readiness-watch creation.
/// 3. IPC server started (`tokio::spawn`) — socket bound HERE, before auth.
/// 4. Wait for socket to appear (fast: < 100 ms).
/// 5. Lock handoff with spawner (if launched via connect_or_spawn).
/// 6. Local Provider model state is prepared while the socket remains available.
/// 7. `ready_tx.send(true)` — unblocks ACP forwarding in the IPC server.
/// 8. LocalSet: agent, IPC bridges, and config watcher.
///
/// # Arguments
///
/// * `agent_config` - The agent configuration
/// * `no_exit_on_disconnect` - If true, the leader will not exit when all clients disconnect
pub async fn run_leader(
    agent_config: &AgentConfig,
    no_exit_on_disconnect: bool,
    _relay_on_demand: bool,
    memory_config: Option<crate::config::MemoryConfig>,
) -> anyhow::Result<()> {
    use crate::leader::{
        LeaderLock, LeaderServerControlState, LeaderServerMetadata, ShutdownReason,
        compute_ws_url_suffix, run_leader_server,
    };
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    register_fs_watch_runtime();
    atelier_telemetry::unified_log::set_version(atelier_version::VERSION);

    let mut agent_config = agent_config.clone();
    agent_config.mode = crate::agent::config::AgentMode::Leader;

    // Use the WS URL to determine which socket/lock paths to use.
    let ws_url = &agent_config.atelier_com_config.atelier_ws_url;
    let mut lock = LeaderLock::new(ws_url);
    let socket_path = lock.socket_path().clone();

    // Early bail-out: lock held + socket exists → another leader is running.
    //
    // Three cases:
    // - Lock free              → we ARE the leader; hold lock through setup.
    // - Lock held + socket     → another leader running → bail out immediately.
    // - Lock held + no socket  → spawner (connect_or_spawn) holds lock and is
    //                            waiting for our socket → proceed normally.
    let lock_already_held = match lock.try_acquire() {
        Ok(true) => {
            lock.write_pid()?;
            debug!("Lock acquired immediately, proceeding as leader");
            true
        }
        Ok(false) => {
            if crate::leader::listener_is_ready(&socket_path) {
                info!(
                    "Another leader is already running (lock held, socket exists at {}). Exiting.",
                    socket_path.display()
                );
                return Err(anyhow::anyhow!(
                    "Another leader is already running at {}",
                    socket_path.display()
                ));
            }
            debug!("Lock held by spawner (no socket yet), proceeding with socket-then-lock flow");
            false
        }
        Err(e) => return Err(anyhow::anyhow!("Failed to check leader lock: {}", e)),
    };

    // ── Phase 1: Clean up stale socket ────────────────────────────────────────
    lock.cleanup_socket()?;
    info!("Leader server starting");

    // ── Phase 2: Create all channels + readiness watch ────────────────────────
    //
    // All channels are created here so the IPC server can start receiving
    // client connections immediately, before auth/prefetch begin.

    // IPC ↔ agent channels
    let (ipc_to_agent_tx, mut ipc_to_agent_rx) = mpsc::unbounded_channel::<String>();
    let (agent_to_ipc_tx, agent_to_ipc_rx) = mpsc::unbounded_channel::<String>();

    // ACP simplex streams for the agent connection
    let (acp_incoming_rx, acp_incoming_tx) = simplex(MAX_BUFFER_SIZE);
    let (acp_outgoing_rx, acp_outgoing_tx) = simplex(MAX_BUFFER_SIZE);

    let incoming = acp_incoming_rx.compat();
    let outgoing = acp_outgoing_tx.compat_write();

    // Shared writer used by IPC and local config-watcher injections.
    let acp_incoming_tx = Arc::new(TokioMutex::new(acp_incoming_tx));

    // Cancellation token for the entire leader lifetime.
    let cancel = CancellationToken::new();

    // Readiness watch: IPC server gates ACP forwarding until this is `true`.
    // We hold `ready_tx` here and send `true` after auth + prefetch succeed.
    let (ready_tx, ready_rx) = watch::channel(false);

    // Shutdown-reason watch: default is Manual; the leader's explicit
    // `RelaunchForUpdate` control handler sends AutoUpdate before cancelling so
    // clients receive the correct ShuttingDown reason. The server
    // derives its own receiver from the sender via `subscribe()`, so we only need
    // to keep the sender; `_shutdown_reason_rx` is held to keep the channel open.
    let (shutdown_tx, _shutdown_reason_rx) = watch::channel(ShutdownReason::Manual);

    // Retained as a local client-mode signal for the leader protocol. It has no
    // network side effect in the vendorless runtime.
    let (relay_demand_tx, _relay_demand_rx) = watch::channel(false);

    let client_count = Arc::new(AtomicUsize::new(0));
    let agent_busy = Arc::new(AtomicBool::new(false));
    // Agent-derived activity view for the IPC server's explicit relaunch drain.
    let agent_activity = crate::agent::activity::AgentActivity::default();
    let control_state = LeaderServerControlState::new(LeaderServerMetadata {
        pid: std::process::id(),
        socket_path: socket_path.clone(),
        lock_path: lock.lock_path().clone(),
        ws_url_suffix: compute_ws_url_suffix(ws_url),
        leader_binary_version: atelier_version::VERSION.to_string(),
    })
    .with_default_hub_url(agent_config.hub.url.clone());

    // Cloned before control_state moves into the IPC server; auth wired below.
    let workspace_control = control_state.workspace.clone();

    // ── Phase 3: Bind socket and start IPC server (BEFORE auth/prefetch) ──────
    //
    // Starting the server here means connect_or_spawn sees the socket in < 100 ms
    // regardless of how long auth + model prefetch take. The `ready_rx` gate inside
    // the server ensures early ACP messages get a structured `leader_starting` error
    // rather than hanging or silently dropping.
    let ipc_server_cancel = cancel.clone();
    let socket_path_for_server = socket_path.clone();
    let client_count_for_server = client_count.clone();
    let agent_busy_for_server = agent_busy.clone();
    let agent_activity_for_server = agent_activity.clone();
    let shutdown_tx_for_server = shutdown_tx.clone();
    let ipc_handle = tokio::spawn(async move {
        if let Err(e) = run_leader_server(
            socket_path_for_server,
            ipc_to_agent_tx,
            agent_to_ipc_rx,
            ipc_server_cancel,
            no_exit_on_disconnect,
            client_count_for_server,
            agent_busy_for_server,
            agent_activity_for_server,
            ready_rx,
            relay_demand_tx,
            shutdown_tx_for_server,
            None, // use LEADER_VERSION constant
            control_state,
        )
        .await
        {
            warn!(error = ?e, "Leader server error");
        }
    });

    // ── Phase 4: Wait for socket to appear (fast: < 100 ms now) ──────────────
    let socket_ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !crate::leader::listener_is_ready(&socket_path) {
        if tokio::time::Instant::now() >= socket_ready_deadline {
            cancel.cancel();
            return Err(anyhow::anyhow!(
                "Timeout waiting for IPC socket to be created"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    debug!("IPC socket created");

    // ── Phase 5: Lock handoff ─────────────────────────────────────────────────
    //
    // (a) lock_already_held=true: We acquired the lock at startup. Keep it.
    // (b) lock_already_held=false: spawner holds lock, waiting for our socket.
    //     Now that socket is up, the spawner will see it, connect, and release
    //     the lock. We acquire it here (30 s timeout).
    let _lock = if lock_already_held {
        info!("Leader lock already held from startup, PID already written");
        lock
    } else {
        const LEADER_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
        // spawn_blocking so we don't stall the async runtime while waiting.
        let lock_result = tokio::task::spawn_blocking(move || {
            lock.try_acquire_timeout(LEADER_LOCK_TIMEOUT)?;
            lock.write_pid()?;
            Ok::<_, anyhow::Error>(lock)
        })
        .await;

        match lock_result {
            Ok(Ok(lock)) => {
                info!("Leader lock acquired, PID written");
                lock
            }
            Ok(Err(e)) => {
                warn!(error = ?e, "Failed to acquire leader lock");
                cancel.cancel();
                return Err(anyhow::anyhow!("Failed to acquire leader lock: {}", e));
            }
            Err(e) => {
                warn!(error = ?e, "Lock task panicked");
                cancel.cancel();
                return Err(anyhow::anyhow!("Lock task failed: {}", e));
            }
        }
    };

    // ── Phase 6: Auth + model prefetch ───────────────────────────────────────
    //
    // The IPC server is already accepting connections. Clients that send ACP
    // messages during this window receive a `leader_starting` error and can retry.

    // Model discovery and authentication are local Provider concerns. A
    // detached leader must not read legacy auth or prefetch vendor catalogs.
    let auth: Option<AtelierAuth> = None;

    // ── Phase 6b: Legacy devbox auth migration ─────────────────────────────
    let prefetched_models: Option<IndexMap<String, ModelEntry>> = None;
    let local_runtime_settings: Option<crate::util::config::LocalRuntimeSettings> = None;

    // Process-wide image normalize cache: off by default, toggled here from
    // `LocalRuntimeSettings.image_normalize_cache_enabled` once at startup.
    let image_normalize_cache_enabled = local_runtime_settings
        .as_ref()
        .and_then(|r| r.image_normalize_cache_enabled)
        .unwrap_or(false);
    crate::session::normalize_cache::NormalizeCache::global()
        .set_enabled(image_normalize_cache_enabled);
    tracing::debug!(
        enabled = image_normalize_cache_enabled,
        "image normalize cache toggle resolved from remote settings"
    );

    // ── Phase 7: Signal readiness ─────────────────────────────────────────────
    //
    // Unblocks ACP forwarding inside the IPC server. From this point on, client
    // ACP messages are forwarded to the agent as normal.
    let _ = ready_tx.send(true);
    info!("Leader ready: auth and model prefetch complete, ACP forwarding enabled");

    // ── Phase 8: LocalSet — agent, IPC bridges, config watcher ───────────────

    let local_set = tokio::task::LocalSet::new();
    let local_runtime_settings_for_reloader = local_runtime_settings.clone();
    let mut agent_config_for_spawn = agent_config.clone();
    agent_config_for_spawn.local_runtime_settings = local_runtime_settings;
    crate::util::config::sync_campaign_fields(&mut agent_config_for_spawn);
    let agent_to_ipc_tx_clone = agent_to_ipc_tx.clone();
    let cancel_clone = cancel.clone();

    let shared_auth_manager = Arc::new(agent_config_for_spawn.create_auth_manager());
    // Proactive token refresh for the leader; cancelled on shutdown.
    shared_auth_manager.start_proactive_refresh(cancel_clone.clone());
    // Pause refreshes across system sleep on this local (laptop) leader
    // process so a refresh can't straddle a suspend.
    shared_auth_manager.start_system_power_listener();

    // Same manager as the leader, so the exposure never writes auth.json itself.
    workspace_control.set_auth_manager(shared_auth_manager.clone());
    let auth_manager_for_agent = shared_auth_manager.clone();
    let auth_manager_for_config = shared_auth_manager;

    // Restore managed policy right before bootstrap reads it (no stale window after the long auth/prefetch phase).

    let (agent_config_for_spawn, shared_models_manager) = bootstrap(
        &agent_config_for_spawn,
        &auth_manager_for_agent,
        prefetched_models,
    )
    .unwrap_or_else(exit_on_config_error);
    let models_manager_for_agent = shared_models_manager.clone();
    let models_manager_for_config = shared_models_manager;

    // Resolve `mcp.recursive_config_watch`
    // ONCE here, before the channel is created, so a kill-switch
    // value of `false` skips channel construction entirely. Previously
    // the channel was always created and `tx` always installed on
    // the agent; the drain task only ran when the flag was on, so
    // every `notify_session_cwd_for_watch` call leaked a `PathBuf`
    // into a never-drained channel.
    let recursive_config_watch_enabled = {
        let user_cfg = crate::config::load_from_disk().ok();
        let requirements = crate::agent::config::read_requirements_toml();
        crate::util::config::resolve_mcp_recursive_config_watch(
            requirements.as_ref(),
            user_cfg.as_ref(),
            /* managed */ None,
        )
    };

    local_set
        .run_until(async move {
            // Channel for fanning new session cwds from
            // the agent (each `spawn_and_register_session` call) into
            // the leader's `ConfigFileWatcher::watch_path`. Both ends
            // live inside the `LocalSet` so neither needs `Send`. The
            // tx is installed on the agent before `AgentSideConnection`
            // moves it; the rx is drained by a small task spawned
            // alongside the watcher below.
            //
            // Only create the channel when the kill-
            // switch is `true`. With the flag off,
            // `notify_session_cwd_for_watch` becomes a no-op (no
            // `tx` installed) and no memory leaks regardless of how
            // many sessions spawn over the leader's lifetime.
            let (config_watcher_path_tx, config_watcher_path_rx_opt) =
                if recursive_config_watch_enabled {
                    let (tx, rx) = mpsc::unbounded_channel::<std::path::PathBuf>();
                    (Some(tx), Some(rx))
                } else {
                    (None, None)
                };
            let mut config_watcher_path_rx = config_watcher_path_rx_opt;

            // Spawn the agent
            let agent_config_watcher_path_tx = config_watcher_path_tx.clone();
            let agent_activity_for_agent = agent_activity.clone();
            tokio::task::spawn_local(async move {
                let (gw_tx, gw_rx) = tokio::sync::mpsc::unbounded_channel();
                let gateway = GatewaySender::new(gw_tx);
                let mut agent = MvpAgent::with_models(
                    gateway,
                    &agent_config_for_spawn,
                    auth_manager_for_agent,
                    models_manager_for_agent,
                );
                agent.set_activity(agent_activity_for_agent);
                if let Some(mc) = memory_config {
                    agent.set_memory_config(mc);
                }
                if let Some(tx) = agent_config_watcher_path_tx {
                    agent.set_config_watcher_path_tx(tx);
                }
                let incoming = LineBufferedRead::spawn_local(incoming);
                let (conn, handle_io) =
                    acp::AgentSideConnection::new(agent, outgoing, incoming, |fut| {
                        tokio::task::spawn_local(fut);
                    });
                tokio::task::spawn_local(
                    GatewayReceiver::new(gw_rx, conn)
                        .run(),
                );

                if let Err(e) = handle_io.await {
                    warn!(error = ?e, "Agent I/O handler error");
                }
                info!("Agent task completed");
            });

            // Bridge IPC messages to agent (from stdio clients)
            let acp_incoming_tx_ipc = acp_incoming_tx.clone();
            tokio::task::spawn_local(async move {
                while let Some(msg) = ipc_to_agent_rx.recv().await {
                    let mut tx = acp_incoming_tx_ipc.lock().await;
                    if tx.write_all(msg.as_bytes()).await.is_err()
                        || tx.write_all(b"\n").await.is_err()
                    {
                        warn!("Failed to write IPC message to agent");
                        break;
                    }
                }
            });

            // Bridge agent responses to IPC clients.
            tokio::task::spawn_local(async move {
                let mut reader = BufReader::new(acp_outgoing_rx);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            let msg = line.trim_end_matches(['\r', '\n']).to_string();
                            if !msg.is_empty() {
                                let _ = agent_to_ipc_tx_clone.send(msg);
                            }
                        }
                        Err(e) => {
                            warn!(error = ?e, "Error reading from agent outgoing stream");
                            break;
                        }
                    }
                }
            });

            // Config hot-reload watcher
            let cwd_for_watcher = std::env::current_dir().unwrap_or_default();
            let mut watch_paths = crate::config::find_project_configs(&cwd_for_watcher);
            watch_paths.extend(crate::util::config::mcp_json_candidate_paths(
                &cwd_for_watcher,
            ));
            if let Some(home) = dirs::home_dir() {
                watch_paths.push(home.join(".claude.json"));
            }
            let auth_scope = agent_config.atelier_com_config.auth_scope();
            // Gated on user_atelier_home() so a cwd-relative .atelier/auth.json is never
            // read as the user auth store when no home resolves.
            let initial_auth_key_hash = atelier_config::user_atelier_home()
                .map(|g| g.join("auth.json"))
                .and_then(|auth_path| crate::auth::read_auth_json(&auth_path).ok())
                .and_then(|store| {
                    crate::auth::lookup_auth(&store, &auth_scope)
                        .map(|a| crate::config::reloader::hash_auth_key(&a.key))
                })
                .unwrap_or(0);
            let (config_update_tx, mut config_update_rx) =
                mpsc::unbounded_channel::<crate::config::reloader::ConfigUpdate>();

            // `mcp.recursive_config_watch` (default
            // `true`) was resolved above (before the async block) so
            // the per-session-cwd channel could be gated. The
            // watcher passes `Some(cwd)` here only when the flag is
            // on. When disabled, behavior reverts to the prior
            // default: only explicit `extra_paths` are watched (kill
            // switch for the rollout).
            let watcher_cwd = recursive_config_watch_enabled.then_some(cwd_for_watcher.as_path());

            let _config_watcher = if let Some((watcher, events_rx)) =
                crate::config::watcher::ConfigFileWatcher::start(
                    &atelier_home::atelier_home(),
                    &watch_paths,
                    watcher_cwd,
                    None,
                ) {
                // Share ownership between the leader's
                // long-lived binding and the per-cwd dynamic
                // registration drain task. `Rc<RefCell<>>` is safe
                // because both ends live inside the leader's
                // `LocalSet` — the watcher type is not `Sync`-needed.
                let watcher = std::rc::Rc::new(std::cell::RefCell::new(watcher));

                // Dynamic registration drain. Lives only
                // when the recursive_config_watch flag is on AND the
                // OS watcher started. With the flag
                // off the channel itself was never created, so
                // there's no rx to drain and no `PathBuf` ever
                // queued (no leak).
                if let Some(mut rx) = config_watcher_path_rx.take() {
                    let cancel_for_drain = cancel_clone.clone();
                    let watcher_for_drain = watcher.clone();
                    tokio::task::spawn_local(async move {
                        loop {
                            tokio::select! {
                                biased;
                                _ = cancel_for_drain.cancelled() => break,
                                cwd = rx.recv() => match cwd {
                                    Some(cwd) => watcher_for_drain.borrow_mut().watch_path(&cwd),
                                    None => break,
                                },
                            }
                        }
                    });
                }
                let initial_config = crate::config::load_effective_config()
                    .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));
                let reloader = crate::config::reloader::ConfigReloader::new(
                    atelier_home::atelier_home(),
                    initial_auth_key_hash,
                    initial_config,
                    auth_scope,
                    local_runtime_settings_for_reloader,
                    config_update_tx,
                    agent_config.cli_experimental_memory,
                    agent_config.cli_no_memory,
                );
                tokio::spawn(reloader.run(events_rx, cancel_clone.clone()));
                Some(watcher)
            } else {
                warn!("Config file watcher failed to start; hot-reload disabled");
                None
            };

            let _skills_watcher =
                spawn_skills_file_watcher(&acp_incoming_tx, &agent_config.skills.paths);

            let ipc_tx_for_config = agent_to_ipc_tx.clone();
            let acp_tx_for_config = acp_incoming_tx.clone();
            tokio::task::spawn_local(async move {
                use crate::config::reloader::ConfigUpdate;
                while let Some(update) = config_update_rx.recv().await {
                    match update {
                        ConfigUpdate::Auth(auth) => {
                            info!(
                                key_len = auth.key.len(),
                                expires_at = ?auth.expires_at,
                                "Auth token hot-reloaded from config watcher"
                            );
                            atelier_telemetry::unified_log::info(
                                "auth hot-swapped from disk",
                                None,
                                Some(serde_json::json!({
                                    "key_len": auth.key.len(),
                                    "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
                                })),
                            );
                            auth_manager_for_config.hot_swap(*auth);
                            models_manager_for_config.on_auth_changed().await;
                            let line = internal_reload_request_line(
                                "config-auth-reloaded",
                                "atelier/internal/reload_all_mcp_servers",
                                serde_json::json!({}),
                            );
                            let mut tx = acp_tx_for_config.lock().await;
                            if let Err(e) = tx.write_all(line.as_bytes()).await {
                                warn!(error = %e, "failed to inject MCP reload after auth hot-swap");
                            }
                        }
                        ConfigUpdate::AuthCleared => {
                            auth_manager_for_config.clear_in_memory();
                            let line = internal_reload_request_line(
                                "config-auth-cleared",
                                "atelier/internal/auth_cleared",
                                serde_json::json!({}),
                            );
                            let mut tx = acp_tx_for_config.lock().await;
                            if let Err(e) = tx.write_all(line.as_bytes()).await {
                                warn!(error = %e, "failed to inject auth-cleared cleanup into ACP stream");
                            }
                            models_manager_for_config.on_auth_changed().await;
                            atelier_telemetry::unified_log::warn(
                                "auth cleared from disk",
                                None,
                                None,
                            );
                            info!("Auth cleared by config watcher");
                        }
                        ConfigUpdate::McpServersChanged => {
                            info!("MCP server config change detected — reloading active sessions");
                            let line = internal_reload_request_line(
                                "config-reload-mcp",
                                "atelier/internal/reload_all_mcp_servers",
                                serde_json::json!({}),
                            );
                            let mut tx = acp_tx_for_config.lock().await;
                            if let Err(e) = tx.write_all(line.as_bytes()).await {
                                warn!(error = %e, "failed to inject MCP reload into ACP stream");
                            }
                        }
                        ConfigUpdate::ProjectMcpServersChanged { cwd } => {
                            // Scope the reload to
                            // sessions whose cwd matches `cwd` (or is
                            // a descendant). The actual filtering
                            // happens in
                            // `handle_reload_project_mcp_servers`
                            // (extensions/session_admin.rs) — this
                            // arm just injects the ACP method with
                            // the cwd as a param.
                            info!(
                                cwd = %cwd.display(),
                                "project MCP config change detected — reloading matching sessions"
                            );
                            let line = internal_reload_request_line(
                                "config-reload-project-mcp",
                                "atelier/internal/reload_project_mcp_servers",
                                serde_json::json!({ "cwd": cwd.to_string_lossy() }),
                            );
                            let mut tx = acp_tx_for_config.lock().await;
                            if let Err(e) = tx.write_all(line.as_bytes()).await {
                                warn!(
                                    error = %e,
                                    "failed to inject project MCP reload into ACP stream"
                                );
                            }
                        }
                        ConfigUpdate::ModelsChanged => {
                            info!("Model config change detected — reloading agent model list");
                            let line = internal_reload_request_line(
                                "config-reload-models",
                                "atelier/internal/reload_models",
                                serde_json::json!({}),
                            );
                            let mut tx = acp_tx_for_config.lock().await;
                            if let Err(e) = tx.write_all(line.as_bytes()).await {
                                warn!(error = %e, "failed to inject model reload into ACP stream");
                            }
                        }
                        ConfigUpdate::ModelsCacheChanged => {
                            // External write to ~/.atelier/models_cache.json
                            // (another atelier process fetched a fresher /v1/models
                            // catalog). Injected into the agent's ACP stream —
                            // NOT applied directly on the manager — so it is
                            // serialized behind any `reload_models` from the
                            // same watcher batch: the `ModelsChanged` arm above
                            // only *injects* a request that completes
                            // asynchronously, and a direct call here could
                            // rebuild the catalog and notify clients before
                            // `apply_config` decided to accept or reject the
                            // new config. The agent processes stream requests
                            // in order, eliminating that interleaving.
                            // `reload_from_disk_cache` still content-dedupes
                            // the leader's own cache writes.
                            info!("Models cache change detected — reloading agent model catalog");
                            let line = internal_reload_request_line(
                                "config-reload-models-cache",
                                "atelier/internal/reload_models_cache",
                                serde_json::json!({}),
                            );
                            let mut tx = acp_tx_for_config.lock().await;
                            if let Err(e) = tx.write_all(line.as_bytes()).await {
                                warn!(
                                    error = %e,
                                    "failed to inject models-cache reload into ACP stream"
                                );
                            }
                        }
                        ConfigUpdate::Memory(mem) => {
                            info!(
                                enabled = mem.enabled,
                                "Memory config change detected by watcher"
                            );
                        }
                        ConfigUpdate::Skills(skills) => {
                            info!(
                                paths = skills.paths.len(),
                                "Skills config change detected by watcher"
                            );
                        }
                        ConfigUpdate::Compat(_compat) => {
                            info!(
                                "Compat config change detected by watcher \
                                 (applies on next agent rebuild)"
                            );
                        }
                        ConfigUpdate::Ui {
                            theme,
                            yolo,
                            fork_secondary_model,
                        } => {
                            info!("UI config change detected by watcher");
                            let notification = serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "atelier/config_changed",
                                "params": {
                                    "section": "ui",
                                    "changes": {
                                        "theme": theme,
                                        "yolo": yolo,
                                        "fork_secondary_model": fork_secondary_model,
                                    }
                                }
                            });
                            let _ = ipc_tx_for_config.send(notification.to_string());
                        }
                    }
                }
            });

            // Wait for IPC server shutdown or cancellation.
            // ipc_handle is a JoinHandle from tokio::spawn — awaitable directly.
            tokio::select! {
                biased;
                _ = ipc_handle => {
                    info!("IPC server stopped, shutting down leader");
                }
                _ = cancel_clone.cancelled() => {
                    info!("Leader cancelled");
                }
            }

            anyhow::Ok(())
        })
        .await?;

    // Brief grace period for the upload queue worker to finish in-flight uploads.
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The watcher-injected internal reload requests must carry the ACP
    /// wire-level `_` extension prefix. `agent-client-protocol`'s inbound
    /// decoder routes non-built-in methods to `ext_method` only when
    /// `_`-prefixed and rejects bare custom methods with `-32601`, so an
    /// un-prefixed injection means every config-driven hot-reload silently
    /// dies at decode (watcher logs fire, handlers never run).
    #[test]
    fn internal_reload_request_line_uses_wire_ext_prefix() {
        let line = internal_reload_request_line(
            "config-reload-models",
            "atelier/internal/reload_models",
            serde_json::json!({}),
        );
        assert!(line.ends_with('\n'), "must be a newline-terminated line");
        let msg: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(
            msg["method"], "_atelier/internal/reload_models",
            "wire method must carry the `_` ext prefix or the ACP decoder \
             rejects it with method_not_found"
        );
        assert_eq!(msg["id"], "config-reload-models");
        assert_eq!(msg["jsonrpc"], "2.0");

        // Params must pass through verbatim (project-MCP reload carries cwd).
        let line = internal_reload_request_line(
            "config-reload-project-mcp",
            "atelier/internal/reload_project_mcp_servers",
            serde_json::json!({ "cwd": "/repo/x" }),
        );
        let msg: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(msg["params"]["cwd"], "/repo/x");

        let line = internal_reload_request_line(
            "config-auth-cleared",
            "atelier/internal/auth_cleared",
            serde_json::json!({}),
        );
        let msg: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(msg["method"], "_atelier/internal/auth_cleared");
    }
}
