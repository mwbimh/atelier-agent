use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use tokio::io::AsyncBufReadExt as _;
use tokio::sync::{mpsc, oneshot};

use crate::auth::config::LEGACY_AUTH_SCOPE;
use crate::auth::{AtelierAuth, AtelierComConfig, AuthManager, parse_output};
use crate::util::atelier_home;

pub type StderrCallback = Box<dyn Fn(&str)>;

/// Reject a cached credential for reuse if it lacks `oidc_issuer`, has a
/// mismatched issuer, or its team principal violates the `force_login_team_uuid`
/// pin — so interactive login starts fresh instead of reusing a stale/wrong-team
/// session.
fn is_cached_credential_compatible(
    auth: &AtelierAuth,
    atelier_com_config: &AtelierComConfig,
) -> bool {
    let expected_issuer = atelier_com_config
        .oidc
        .as_ref()
        .map(|c| c.issuer.as_str())
        .or_else(|| {
            atelier_com_config
                .oauth2
                .as_ref()
                .map(|c| c.issuer.as_str())
        });
    let issuer_compatible = match (auth.oidc_issuer.as_deref(), expected_issuer) {
        (Some(actual), Some(expected)) => actual == expected,
        (None, Some(_)) => false,
        _ => true,
    };
    if !issuer_compatible {
        return false;
    }
    if let Some(policy) = crate::auth::oidc::login_principal_policy(atelier_com_config) {
        let actual = crate::auth::oidc::peek_access_token_principal_id(&auth.key);
        if crate::auth::oidc::enforce_login_principal(Some(&policy), actual.as_deref()).is_err() {
            return false;
        }
    }
    true
}

/// CLI-flag override for the interactive login transport.
///
/// `--oauth` forces the loopback-callback flow; `--device-auth` forces the
/// device flow. `None` falls through to env / config / default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoginTransportOverride {
    /// No CLI override — resolve from env / config / default.
    #[default]
    None,
    /// `--oauth`: force the loopback-callback flow.
    ForceLoopback,
    /// `--device-auth`: force the RFC 8628 device flow.
    ForceDevice,
    /// Transport already resolved (and logged) upstream; the inner flow honors
    /// the carried value (`true` = device, `false` = loopback) without
    /// re-resolving, so it's never re-logged or mis-attributed to `cli`.
    Preresolved(bool),
}

impl LoginTransportOverride {
    /// Resolve from the `--oauth` / `--device-auth` flags. `--oauth` wins if
    /// both are somehow set. Single source of truth for both the CLI
    /// (`run_cli_login`) and ACP (`AuthRequestMeta`) entry points.
    pub fn from_flags(force_loopback: bool, force_device: bool) -> Self {
        if force_loopback {
            Self::ForceLoopback
        } else if force_device {
            Self::ForceDevice
        } else {
            Self::None
        }
    }

    /// Map to a `BoolFlag` CLI value (`Some(true)` = device, `Some(false)` =
    /// loopback, `None` = no override).
    fn as_cli_bool(self) -> Option<bool> {
        match self {
            Self::None => None,
            Self::ForceLoopback => Some(false),
            Self::ForceDevice => Some(true),
            // Not a CLI decision — must never be reported as the `cli` tier.
            Self::Preresolved(_) => None,
        }
    }
}

/// `[auth] login_device_flow` from a config snapshot (shared with the proxy-URL read).
fn config_login_device_flow(effective: Option<&toml::Value>) -> Option<bool> {
    effective.and_then(|cfg| cfg.get("auth")?.get("login_device_flow")?.as_bool())
}

/// Device-flow precedence: CLI > env > config > loopback.
/// Returns the deciding tier so the caller can log which one chose the transport.
fn resolve_device_flow(
    login_override: LoginTransportOverride,
    config: Option<bool>,
    _legacy_remote: Option<bool>,
) -> crate::agent::config::Resolved<bool> {
    crate::agent::config::BoolFlag::env("ATELIER_LOGIN_DEVICE_FLOW")
        .cli(login_override.as_cli_bool())
        .config(config)
        .default(false)
        .resolve()
}

/// Whether `run_cli_login` should use the device flow for `config`: only the
/// xAI OAuth2 provider supports it. Enterprise OIDC (`oidc=Some`) always uses
/// the loopback flow, mirroring `run_auth_flow_inner`'s precedence.
async fn cli_should_use_device(
    config: &AtelierComConfig,
    login_override: LoginTransportOverride,
) -> bool {
    !crate::auth::oidc::is_configured(config) && should_use_device_flow(login_override).await
}

/// Whether interactive xAI OAuth2 login uses the RFC 8628 device flow (vs loopback).
///
/// Precedence: CLI (`--oauth`/`--device-auth`) > `ATELIER_LOGIN_DEVICE_FLOW` env >
/// `[auth] login_device_flow` config > loopback.
async fn should_use_device_flow(login_override: LoginTransportOverride) -> bool {
    // Already resolved (and logged) upstream — honor it without re-resolving or
    // emitting a second transport log.
    if let LoginTransportOverride::Preresolved(use_device) = login_override {
        return use_device;
    }
    let resolved = if login_override.as_cli_bool().is_some() {
        // CLI flag wins outright, so skip the config load and the remote settings fetch.
        resolve_device_flow(login_override, None, None)
    } else {
        // Atelier is vendorless: login transport is decided locally and must
        // never consult the removed vendor feature-flag endpoint.
        let effective = crate::config::load_effective_config().ok();
        let config = config_login_device_flow(effective.as_ref());
        resolve_device_flow(login_override, config, None)
    };
    tracing::info!(
        transport = if resolved.value { "device" } else { "loopback" },
        source = %resolved.source,
        "login: resolved interactive transport",
    );
    resolved.value
}

/// How login presents itself; surfaced to the TUI via `atelier/auth/get_url`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthUrlMode {
    /// Loopback-callback flow — TUI shows a copyable URL + paste box.
    Loopback,
    /// External auth provider opened its own browser — TUI shows a waiting status.
    Command,
    /// RFC 8628 device flow — TUI shows the device code + copyable URL, no paste box.
    Device,
}

impl AuthUrlMode {
    /// Wire string for the `atelier/auth/get_url` ACP response.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Command => "command",
            Self::Device => "device",
        }
    }

    /// Back-compat flag for older clients that only read `external_provider`.
    pub fn is_external_provider(self) -> bool {
        matches!(self, Self::Command)
    }
}

/// Auth URL pushed from the auth flow to the TUI.
pub struct AuthUrlInfo {
    pub url: String,
    pub mode: AuthUrlMode,
}

/// Channels for interactive login between the auth flow and the TUI/extension.
pub struct AuthChannels {
    pub url_tx: Option<oneshot::Sender<AuthUrlInfo>>,
    pub code_rx: mpsc::Receiver<String>,
}

async fn run_external_auth_provider(
    command: &str,
    auth_manager: &Arc<AuthManager>,
    is_refresh: bool,
    on_stderr: Option<StderrCallback>,
) -> anyhow::Result<(AtelierAuth, bool)> {
    let inherit_stderr = on_stderr.is_none();
    tracing::info!(
        cmd = %command,
        is_refresh,
        inherit_stderr,
        "auth: running external auth provider"
    );

    let mut cmd = tokio::process::Command::from(super::external_auth::shell_command(command));
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true);

    // TUI: pipe stderr and forward via callback — inherit would corrupt the
    // alternate screen. CLI / headless: inherit so URLs and progress appear in
    // real time; piping without a reader hides output and can deadlock the child.
    if inherit_stderr {
        cmd.stderr(std::process::Stdio::inherit());
    } else {
        cmd.stderr(std::process::Stdio::piped());
    }

    if is_refresh {
        cmd.env("ATELIER_AUTH_EXPIRED", "1");
    }

    atelier_tools::util::detach_command(&mut cmd);
    cmd.envs(atelier_tools::util::pager_env());

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start auth provider `{command}`: {e}"))?;

    let stderr_task = if let Some(cb) = on_stderr {
        let stderr = child.stderr.take().expect("stderr was set to piped");
        Some(tokio::task::spawn_local(async move {
            let mut reader = tokio::io::BufReader::new(stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end();
                        tracing::debug!(line = trimmed, "auth: provider stderr");
                        cb(trimmed);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "auth: error reading provider stderr");
                        break;
                    }
                }
            }
        }))
    } else {
        None
    };

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("external auth provider `{command}` timed out after 300s"))?
    .map_err(|e| anyhow::anyhow!("external auth provider `{command}` IO error: {e}"))?;

    if let Some(task) = stderr_task {
        let _ = task.await;
    }

    let mut auth = parse_output(&output)
        .map_err(|e| anyhow::anyhow!("external auth provider `{command}`: {e}"))?;

    // Verify the team pin before any persist (parity with the OIDC / device-code
    // completion paths). A mismatch fails the login and writes nothing.
    let principal_policy =
        crate::auth::oidc::login_principal_policy(auth_manager.atelier_com_config());
    crate::auth::oidc::enforce_login_principal(
        principal_policy.as_ref(),
        crate::auth::oidc::peek_access_token_principal_id(&auth.key).as_deref(),
    )?;

    // Token output has no profile; carry it forward, or fetch it when reauth cleared prev.
    match (is_refresh, auth_manager.current_or_expired()) {
        (true, Some(prev)) => auth.carry_user_profile_from(&prev),
        _ => auth_manager.enrich_auth_inline(&mut auth).await,
    }

    let auth = auth_manager
        .update(auth)
        .await
        .map_err(|e| anyhow::anyhow!("failed to save external auth credentials: {e}"))?;

    tracing::info!(
        user_id = %auth.user_id,
        email = ?auth.email,
        "auth: external provider login complete"
    );

    Ok((auth, true))
}

/// GUI auth: bridges external provider stderr to `url_tx`, pipes code submission via `code_rx`.
pub async fn run_auth_flow_with_stderr_bridge(
    auth_manager: &Arc<AuthManager>,
    atelier_com_config: &AtelierComConfig,
    channels: AuthChannels,
    reauth: bool,
    force_interactive: bool,
    login_override: LoginTransportOverride,
) -> anyhow::Result<(AtelierAuth, bool)> {
    if auth_manager.is_local_only() {
        anyhow::bail!("vendor authentication is disabled; configure a local Provider");
    }

    let url_tx = Rc::new(RefCell::new(channels.url_tx));
    let stderr_lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    let writer = stderr_lines.clone();
    let on_stderr: StderrCallback = Box::new(move |line: &str| {
        writer.borrow_mut().push(line.to_owned());
    });

    let reader = stderr_lines.clone();
    let url_tx_bridge = url_tx.clone();
    let bridge = async move {
        loop {
            tokio::task::yield_now().await;
            let content = {
                let lines = reader.borrow();
                if lines.is_empty() {
                    None
                } else {
                    Some(lines.join("\n"))
                }
            };
            if let Some(joined) = content
                && let Some(tx) = url_tx_bridge.borrow_mut().take()
            {
                // The external binary may print preamble text alongside the
                // URL (e.g. "Visit the following link to sign in: https://…").
                // Extract just the first https:// URL so the TUI displays a
                // clean, clickable link.
                let url = joined
                    .split_whitespace()
                    .find(|w| w.starts_with("https://"))
                    .map(|u| u.to_owned())
                    .unwrap_or(joined);
                let _ = tx.send(AuthUrlInfo {
                    url,
                    mode: AuthUrlMode::Command,
                });
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };

    if force_interactive {
        let auth = run_auth_flow_interactive(
            auth_manager,
            atelier_com_config,
            Some(on_stderr),
            Some(url_tx),
            Some(channels.code_rx),
            login_override,
        );
        tokio::select! {
            r = auth => r,
            _ = bridge => {
                tracing::error!("auth stderr bridge exited unexpectedly during interactive login");
                Err(anyhow::anyhow!("Login failed. Please try again."))
            },
        }
    } else {
        let auth = run_auth_flow(
            auth_manager,
            atelier_com_config,
            reauth,
            Some(on_stderr),
            Some(url_tx),
            Some(channels.code_rx),
            login_override,
        );
        tokio::select! {
            r = auth => r,
            _ = bridge => {
                tracing::error!("auth stderr bridge exited unexpectedly during login");
                Err(anyhow::anyhow!("Login failed. Please try again."))
            },
        }
    }
}

/// Full auth chain: cache → refresh → external provider → interactive (OIDC/OAuth2/legacy).
/// When `url_tx` and `code_rx` are `None`, falls back to stderr/stdin (CLI mode).
pub async fn run_auth_flow(
    auth_manager: &Arc<AuthManager>,
    atelier_com_config: &AtelierComConfig,
    reauth: bool,
    on_stderr: Option<StderrCallback>,
    url_tx: Option<Rc<RefCell<Option<oneshot::Sender<AuthUrlInfo>>>>>,
    code_rx: Option<mpsc::Receiver<String>>,
    login_override: LoginTransportOverride,
) -> anyhow::Result<(AtelierAuth, bool)> {
    if auth_manager.is_local_only() {
        anyhow::bail!("vendor authentication is disabled; configure a local Provider");
    }
    run_auth_flow_inner(
        auth_manager,
        atelier_com_config,
        reauth,
        false,
        on_stderr,
        url_tx,
        code_rx,
        login_override,
    )
    .await
}

/// Like [`run_auth_flow`] but with `force_interactive`: skip cached
/// credentials without clearing them. Used by `/login` for mid-session
/// re-auth where abandoning the flow must not disrupt the session.
pub async fn run_auth_flow_interactive(
    auth_manager: &Arc<AuthManager>,
    atelier_com_config: &AtelierComConfig,
    on_stderr: Option<StderrCallback>,
    url_tx: Option<Rc<RefCell<Option<oneshot::Sender<AuthUrlInfo>>>>>,
    code_rx: Option<mpsc::Receiver<String>>,
    login_override: LoginTransportOverride,
) -> anyhow::Result<(AtelierAuth, bool)> {
    if auth_manager.is_local_only() {
        anyhow::bail!("vendor authentication is disabled; configure a local Provider");
    }
    run_auth_flow_inner(
        auth_manager,
        atelier_com_config,
        false,
        true,
        on_stderr,
        url_tx,
        code_rx,
        login_override,
    )
    .await
}

async fn run_auth_flow_inner(
    auth_manager: &Arc<AuthManager>,
    atelier_com_config: &AtelierComConfig,
    reauth: bool,
    force_interactive: bool,
    on_stderr: Option<StderrCallback>,
    url_tx: Option<Rc<RefCell<Option<oneshot::Sender<AuthUrlInfo>>>>>,
    code_rx: Option<mpsc::Receiver<String>>,
    login_override: LoginTransportOverride,
) -> anyhow::Result<(AtelierAuth, bool)> {
    tracing::info!(
        has_oidc = atelier_com_config.oidc.is_some(),
        has_oauth2 = atelier_com_config.oauth2.is_some(),
        has_external_auth = atelier_com_config.auth_provider_command.is_some(),
        reauth,
        "auth: starting auth flow"
    );

    if reauth {
        auth_manager.clear()?;
        // Also remove the legacy accounts.x.ai scope so stale tokens
        // don't linger alongside the fresh OIDC credential.
        let _ = auth_manager.remove_scope(LEGACY_AUTH_SCOPE);
    }

    if !force_interactive && let Some(auth) = auth_manager.current() {
        if is_cached_credential_compatible(&auth, atelier_com_config) {
            tracing::info!(auth_mode = ?auth.auth_mode, "auth: using cached credentials");
            atelier_telemetry::unified_log::info(
                "auth: using cached credentials",
                None,
                Some(serde_json::json!({ "auth_mode": format!("{:?}", auth.auth_mode) })),
            );
            return Ok((auth, false));
        }
        tracing::info!(
            auth_mode = ?auth.auth_mode,
            "auth: cached credential incompatible with requested flow, proceeding to interactive login"
        );
        // Remove the stale legacy credential from disk so it doesn't
        // linger alongside the new OIDC entry after re-authentication.
        if auth.auth_mode == super::AuthMode::WebLogin
            && let Err(e) = auth_manager.remove_scope(LEGACY_AUTH_SCOPE)
        {
            tracing::warn!(error = ?e, "auth: failed to remove legacy scope entry (non-fatal)");
        }
    }

    if !force_interactive && !reauth && auth_manager.is_expired() {
        // Acquire the cross-process file lock so we don't race with
        // OidcRefresher instances in sibling processes. Without this,
        // two processes can send the same refresh_token simultaneously,
        // triggering IdP refresh-token-family revocation (reuse detection).
        let _file_lock = auth_manager
            .try_lock_auth_file_async(crate::auth::manager::AUTH_LOCK_TIMEOUT)
            .await;

        // Read disk first — another process may have already refreshed.
        let disk_auth = auth_manager.read_disk_auth();
        let disk_expired = disk_auth.as_ref().is_some_and(crate::auth::is_expired);
        atelier_telemetry::unified_log::info(
            "auth run_auth_flow expired path",
            None,
            Some(serde_json::json!({
                "got_lock": _file_lock.is_some(),
                "disk_found": disk_auth.is_some(),
                "disk_expired": disk_expired,
            })),
        );
        if disk_auth.as_ref().is_some_and(|d| {
            !crate::auth::is_expired(d) && is_cached_credential_compatible(d, atelier_com_config)
        }) {
            atelier_telemetry::unified_log::info(
                "auth run_auth_flow using valid disk token",
                None,
                None,
            );
            let d = disk_auth.unwrap();
            let ret = d.clone();
            auth_manager.hot_swap(d);
            return Ok((ret, false));
        }

        // Disk token not usable. Try the full auth() dispatcher which
        // handles OIDC refresh, external binary, disk re-read — all
        // through refresh_chain (single mutation point).
        match auth_manager.auth().await {
            Ok(fresh) => return Ok((fresh, false)),
            Err(e) => {
                // Defer to consumer-level refresh if disk has a refresh_token.
                if let Some(d) = disk_auth.filter(|d| {
                    matches!(
                        &e,
                        crate::auth::error::AuthError::Refresh(
                            crate::auth::error::RefreshTokenError::Transient(_)
                        )
                    ) && d.refresh_token.is_some()
                }) {
                    atelier_telemetry::unified_log::warn(
                        "auth run_auth_flow refresh failed, deferring to consumer refresh",
                        None,
                        Some(serde_json::json!({
                            "error": format!("{e}"),
                        })),
                    );
                    let ret = d.clone();
                    auth_manager.hot_swap(d);
                    return Ok((ret, false));
                }
                atelier_telemetry::unified_log::warn(
                    "auth run_auth_flow refresh failed, falling through to interactive",
                    None,
                    Some(serde_json::json!({
                        "error": format!("{e}"),
                    })),
                );
            }
        }
    }

    if let Some(ref cmd) = atelier_com_config.auth_provider_command {
        let is_refresh = reauth || auth_manager.is_expired();
        match run_external_auth_provider(cmd, auth_manager, is_refresh, on_stderr).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "auth: external auth provider failed, falling through to interactive login"
                );
                eprintln!("Signing in with browser instead...");
            }
        }
    }

    // Devbox auto-migration: before interactive login (which requires a
    // browser and won't work on headless devboxes), try minting OIDC
    // credentials via the remote devbox login helper.
    // preferred_method=api_key: never auto-mint OIDC (fail-closed). Explicit
    // `atelier login --devbox` uses run_devbox_login and is not gated here.
    if !atelier_com_config.blocks_automatic_oidc()
        && crate::auth::devbox_login::is_devbox_environment()
    {
        tracing::info!("auth: devbox detected, attempting devbox login before interactive flow");
        match crate::auth::devbox_login::mint_devbox_auth(auth_manager).await {
            Ok(new_auth) => match auth_manager.save_without_enrichment(new_auth).await {
                Ok(auth) => {
                    let _ = auth_manager.remove_scope(LEGACY_AUTH_SCOPE);
                    atelier_telemetry::unified_log::info(
                        "auth: devbox migration in auth flow succeeded",
                        None,
                        Some(serde_json::json!({
                            "user_id": auth.user_id,
                            "auth_mode": format!("{:?}", auth.auth_mode),
                        })),
                    );
                    return Ok((auth, true));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "auth: devbox migration save failed in auth flow");
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "auth: devbox login failed, falling through to interactive");
            }
        }
    }

    let url_tx = url_tx.and_then(|rc| rc.borrow_mut().take());
    let mut channels = code_rx.map(|code_rx| AuthChannels { url_tx, code_rx });

    // Enterprise OIDC keeps loopback (customer IdPs may lack a device endpoint).
    // xAI OAuth2 also defaults to loopback; the device flow (robust on
    // remote/SSH where the loopback redirect can't reach the CLI) is opt-in via
    // --device-auth / ATELIER_LOGIN_DEVICE_FLOW / [auth] login_device_flow.
    if crate::auth::oidc::is_configured(atelier_com_config) {
        return crate::auth::oidc::run_login_flow(atelier_com_config, auth_manager, channels).await;
    }

    if let Some(ref oauth2_cfg) = atelier_com_config.oauth2 {
        if should_use_device_flow(login_override).await {
            // On `NotEnabled` (no device endpoint) `channels` is untouched,
            // so we can fall back to loopback below.
            match crate::auth::device_code::run_device_code_login_channels(
                &oauth2_cfg.issuer,
                &oauth2_cfg.client_id,
                &oauth2_cfg.scopes,
                auth_manager,
                &mut channels,
            )
            .await
            {
                Err(e)
                    if matches!(
                        e.downcast_ref::<crate::auth::device_code::DeviceCodeError>(),
                        Some(crate::auth::device_code::DeviceCodeError::NotEnabled)
                    ) =>
                {
                    tracing::warn!(
                        "auth: device flow unavailable (404), falling back to loopback login"
                    );
                }
                other => return other,
            }
        }
        return crate::auth::oidc::run_login_flow_with_config(
            &oauth2_cfg.as_oidc(),
            auth_manager,
            channels,
        )
        .await;
    }

    tracing::error!(
        "auth: no OAuth2 configuration available (neither enterprise OIDC nor xAI OAuth2 configured)"
    );
    anyhow::bail!(
        "No OAuth2 configuration available. Run `atelier login` to authenticate, or contact your administrator if you use enterprise SSO."
    )
}

/// Non-interactive auth refresh: returns valid credentials if available without
/// ever triggering interactive login (browser, device code, etc.).
///
/// Tries in order:
/// 1. Cached credentials (non-expired)
/// 2. OIDC silent refresh (if expired token has a refresh_token)
/// 3. External auth provider command (if configured)
///
/// Returns `None` when no valid credentials can be obtained non-interactively.
pub async fn try_ensure_fresh_auth(atelier_com_config: &AtelierComConfig) -> Option<AtelierAuth> {
    let _ = atelier_com_config;
    // The private runtime has no vendor session. In particular, do not call
    // `AuthManager::new`: it reads legacy ATELIER_AUTH/auth.json and may
    // configure an external refresh provider before Provider resolution.
    None
}

/// Like `try_ensure_fresh_auth` but also mints on cold start (external provider /
/// devbox, never a browser; may take up to ~300s). For detached modes only.
pub(crate) async fn try_ensure_session_noninteractive(
    atelier_com_config: &AtelierComConfig,
) -> Option<AtelierAuth> {
    let _ = atelier_com_config;
    // No legacy session refresh, external auth-provider command, or devbox
    // minting is permitted in Atelier. Credentials come from Provider.
    None
}

/// A cached, refreshable session (not BYOK/ApiKey). Reached only after fresh
/// auth failed, so in practice the token is expired but recoverable on 401.
#[cfg(any())] // The vendor session relay was removed; local Providers own credentials.
fn expired_refreshable_session(auth_manager: &AuthManager) -> Option<AtelierAuth> {
    auth_manager
        .current_or_expired()
        .filter(|a| a.is_configured_refresh_auth() && a.refresh_token.is_some())
}

/// Cold-start mint via non-interactive providers (external command, devbox);
/// `None` when none is available.
#[cfg(any())] // Non-interactive vendor session minting was removed from Atelier startup.
async fn mint_session_noninteractive(
    auth_manager: &Arc<AuthManager>,
    atelier_com_config: &AtelierComConfig,
) -> Option<AtelierAuth> {
    // preferred_method=api_key: never auto-mint OIDC (fail-closed).
    if atelier_com_config.blocks_automatic_oidc() {
        tracing::debug!(
            "mint_session_noninteractive: skipped (preferred_method=api_key blocks automatic OIDC)"
        );
        return None;
    }

    if let Some(cmd) = atelier_com_config.auth_provider_command.as_deref() {
        match run_external_auth_provider(cmd, auth_manager, false, None).await {
            Ok((auth, _)) => return Some(auth),
            Err(e) => {
                tracing::debug!(error = %e, "mint_session_noninteractive: external provider failed");
            }
        }
    }

    if crate::auth::devbox_login::is_devbox_environment() {
        match crate::auth::devbox_login::mint_devbox_auth(auth_manager).await {
            Ok(new_auth) => return Some(persist_or_use_minted(auth_manager, new_auth).await),
            Err(e) => {
                tracing::debug!(error = %e, "mint_session_noninteractive: devbox mint failed");
            }
        }
    }

    None
}

/// Persist a minted token; on persist failure, return it unpersisted rather
/// than dropping a valid credential.
async fn persist_or_use_minted(auth_manager: &AuthManager, new_auth: AtelierAuth) -> AtelierAuth {
    match auth_manager.save_without_enrichment(new_auth.clone()).await {
        Ok(auth) => {
            let _ = auth_manager.remove_scope(LEGACY_AUTH_SCOPE);
            auth
        }
        Err(e) => {
            tracing::warn!(error = %e, "mint persist failed; using unpersisted token");
            new_auth
        }
    }
}

/// Print the CLI "signed in" confirmation, clearing the spinner line first.
fn report_signed_in(auth: &AtelierAuth) {
    eprint!("\r\x1b[K");
    match auth.email {
        Some(ref email) => eprintln!("✓ Signed in as {email}"),
        None => eprintln!("✓ Signed in"),
    }
}

/// CLI auth entrypoint. For GUI, use `run_auth_flow_with_stderr_bridge`.
pub async fn ensure_authenticated(
    atelier_com_config: &AtelierComConfig,
    reauth: bool,
    message_prefix: Option<&str>,
) -> anyhow::Result<AtelierAuth> {
    ensure_authenticated_with_override(
        atelier_com_config,
        reauth,
        message_prefix,
        LoginTransportOverride::None,
    )
    .await
}

/// Like [`ensure_authenticated`] but with an explicit login-transport override
/// (from `--oauth` / `--device-auth`). Used by `run_cli_login`.
pub async fn ensure_authenticated_with_override(
    _atelier_com_config: &AtelierComConfig,
    _reauth: bool,
    _message_prefix: Option<&str>,
    _login_override: LoginTransportOverride,
) -> anyhow::Result<AtelierAuth> {
    anyhow::bail!("vendor authentication is disabled; configure a local Provider");
}

/// Decides *whether to prompt* for an interactive login (the wire credential is
/// chosen separately by `ShellAuthCredentialProvider`).
///
/// With `has_noninteractive_auth`, only refresh a cached token best-effort (no
/// browser, no cold mint); otherwise require an interactive login.
pub async fn ensure_authenticated_or_noninteractive(
    atelier_com_config: &AtelierComConfig,
    has_noninteractive_auth: bool,
    message_prefix: Option<&str>,
) -> anyhow::Result<Option<AtelierAuth>> {
    if has_noninteractive_auth {
        Ok(try_ensure_fresh_auth(atelier_com_config).await)
    } else {
        ensure_authenticated(atelier_com_config, false, message_prefix)
            .await
            .map(Some)
    }
}

/// Unified `atelier login` handler for CLI entry points (tui, pager).
///
/// Vendor authentication is intentionally unavailable. Configure a local
/// Provider instead.
pub async fn run_cli_login(
    _config: &crate::agent::config::Config,
    _oauth: bool,
    _device_auth: bool,
    _devbox: bool,
) -> anyhow::Result<()> {
    anyhow::bail!("vendor authentication is disabled; configure a local Provider");
}

/// Result of a logout operation. Used by both the CLI subcommand and
/// the ACP `/logout` slash command so the presentation layer can format
/// the outcome without duplicating the auth logic.
pub struct LogoutResult {
    /// `true` if a cached OAuth session was found and cleared.
    pub was_logged_in: bool,
    /// Email of the session that was cleared (if available).
    pub email: Option<String>,
}

/// Core logout logic shared by the CLI subcommand and the ACP handler.
///
/// When `scope` is `None`, clears the default scope (same as `/logout`
/// in the TUI). When `Some`, removes only that scope entry.
pub fn perform_logout(
    auth_manager: &AuthManager,
    scope: Option<&str>,
) -> std::io::Result<LogoutResult> {
    let auth = auth_manager.current_or_expired();
    let email = auth.as_ref().and_then(|a| a.email.clone());
    let was_logged_in = auth.is_some();
    // Intentional credential removal must be attributable in
    // unified.jsonl, so a later "auth.json entry gone" can be
    // distinguished from accidental loss (deleted/corrupt file).
    atelier_telemetry::unified_log::info(
        "auth: logout",
        None,
        Some(serde_json::json!({
            "was_logged_in": was_logged_in,
            "scope": scope.unwrap_or("(current)"),
            "user_id": auth.as_ref().map(|a| a.user_id.clone()),
        })),
    );
    if was_logged_in {
        if let Some(scope) = scope {
            auth_manager.remove_scope(scope)?;
        } else {
            auth_manager.clear()?;
        }
        // Clear the synced files if no principal remains to own them. A scoped
        // logout that leaves a team (or a deployment key) signed in keeps them.
        crate::managed_config::clear_orphan();
    }
    Ok(LogoutResult {
        was_logged_in,
        email,
    })
}

/// `atelier logout` CLI handler. Calls [`perform_logout`] and formats
/// the result to stderr.
pub fn run_cli_logout(_config: &crate::agent::config::Config) -> anyhow::Result<()> {
    anyhow::bail!("vendor logout is disabled; Provider credentials are managed locally");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthMode;
    use crate::auth::TEST_OIDC_ISSUER;
    use crate::env::EnvVarGuard;
    use chrono::Utc;
    use serial_test::serial;

    fn emit_command(value: &str) -> String {
        #[cfg(windows)]
        {
            format!("echo {value}")
        }
        #[cfg(not(windows))]
        {
            format!("printf '%s' '{value}'")
        }
    }

    fn large_stderr_command() -> &'static str {
        #[cfg(windows)]
        {
            // `cmd.exe /S /C` owns the outer command-line parsing. Quoting the
            // entire PowerShell script here makes `/S` strip/reinterpret those
            // quotes and PowerShell can receive the script as literal output.
            // `-Command` consumes the remaining command line, so no nested
            // quotes are needed for this script.
            r#"powershell.exe -NoProfile -NonInteractive -Command [Console]::Error.Write('x' * 70000); [Console]::Out.Write('token')"#
        }
        #[cfg(not(windows))]
        {
            r#"i=0; while [ $i -lt 2000 ]; do printf "%s" "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" >&2; i=$((i+1)); done; printf token"#
        }
    }

    /// Run `f` with `ATELIER_LOGIN_DEVICE_FLOW` set to `value` (unset for `None`).
    /// `EnvVarGuard` serializes the process env and restores it on drop, so
    /// `resolve_device_flow` reads the env tier from a known state.
    fn with_device_flow_env<T>(value: Option<bool>, f: impl FnOnce() -> T) -> T {
        let _guard = match value {
            Some(true) => EnvVarGuard::set("ATELIER_LOGIN_DEVICE_FLOW", "true"),
            Some(false) => EnvVarGuard::set("ATELIER_LOGIN_DEVICE_FLOW", "false"),
            None => EnvVarGuard::remove("ATELIER_LOGIN_DEVICE_FLOW"),
        };
        f()
    }

    #[tokio::test]
    #[serial]
    async fn vendorless_auth_ignores_legacy_inline_credentials() {
        let auth = oidc_session("legacy-inline-token", None);
        let auth_json = serde_json::to_string(&auth).expect("test auth should serialize");
        let _auth_env = EnvVarGuard::set("ATELIER_AUTH", &auth_json);

        assert!(
            try_ensure_fresh_auth(&AtelierComConfig::default())
                .await
                .is_none(),
            "vendorless startup must not load legacy inline auth"
        );
        assert!(
            try_ensure_session_noninteractive(&AtelierComConfig::default())
                .await
                .is_none(),
            "vendorless startup must not mint or reuse a legacy session"
        );
    }

    #[tokio::test]
    #[serial]
    async fn vendorless_interactive_auth_is_rejected_before_legacy_load() {
        let auth = oidc_session("legacy-inline-token", None);
        let auth_json = serde_json::to_string(&auth).expect("test auth should serialize");
        let _auth_env = EnvVarGuard::set("ATELIER_AUTH", &auth_json);

        let error = ensure_authenticated_with_override(
            &AtelierComConfig::default(),
            false,
            None,
            LoginTransportOverride::None,
        )
        .await
        .expect_err("vendorless runtime must reject legacy interactive auth");
        assert!(!error.to_string().is_empty());
        assert!(
            error
                .to_string()
                .contains("vendor authentication is disabled"),
            "unexpected error: {error}"
        );
    }

    // A configured OIDC session used by auth-manager tests.
    fn oidc_session(key: &str, refresh: Option<&str>) -> AtelierAuth {
        AtelierAuth {
            key: key.into(),
            auth_mode: AuthMode::Oidc,
            oidc_issuer: Some(TEST_OIDC_ISSUER.to_string()),
            refresh_token: refresh.map(str::to_string),
            ..AtelierAuth::test_default()
        }
    }

    #[cfg(any())] // The vendor session relay was removed; local Providers own credentials.
    #[test]
    fn expired_refreshable_session_gate() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = AuthManager::new(dir.path(), AtelierComConfig::default());

        // Expired but refreshable → returned. Guards a `current_or_expired()` ->
        // `current()` regression that would disable the relay on a transient blip.
        mgr.hot_swap(AtelierAuth {
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            ..oidc_session("expired-but-refreshable", Some("rt"))
        });
        assert!(
            mgr.current().is_none(),
            "precondition: token must be expired"
        );
        assert_eq!(
            expired_refreshable_session(&mgr).map(|a| a.key),
            Some("expired-but-refreshable".to_string())
        );

        // No refresh token → rejected: never hand the relay a token it can't
        // recover on 401 (the gate `for_session` doesn't check this).
        mgr.hot_swap(oidc_session("no-rt", None));
        assert!(expired_refreshable_session(&mgr).is_none());

        // An expired first-party *external* credential with a refresh token
        // is likewise recoverable — 401 recovery re-runs the provider binary
        // (the refresh token is a recoverability marker, not a grant input).
        mgr.hot_swap(AtelierAuth {
            auth_mode: AuthMode::External,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            ..oidc_session("expired-external", Some("rt"))
        });
        assert_eq!(
            expired_refreshable_session(&mgr).map(|a| a.key),
            Some("expired-external".to_string())
        );

        // Third-party external (no x.ai issuer) stays excluded.
        mgr.hot_swap(AtelierAuth {
            oidc_issuer: None,
            auth_mode: AuthMode::External,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            ..oidc_session("expired-external-3p", Some("rt"))
        });
        assert!(expired_refreshable_session(&mgr).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn persist_or_use_minted_returns_token_when_save_fails() {
        use std::os::unix::fs::PermissionsExt;
        // Read-only atelier_home: reading a missing auth.json succeeds (empty), but
        // writing fails — exercising the save-failure path.
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let mgr = Arc::new(AuthManager::new(dir.path(), AtelierComConfig::default()));
        let minted = oidc_session("minted-token", Some("rt"));

        let save = mgr.save_without_enrichment(minted.clone()).await;
        // Root bypasses 0o500, so the write can't be forced to fail there — skip
        // explicitly. Non-root MUST see the save fail (or this proves nothing).
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        assert!(
            save.is_err(),
            "non-root: save into a read-only dir must fail"
        );
        let out = persist_or_use_minted(&mgr, minted).await;
        assert_eq!(
            out.key, "minted-token",
            "must return the unpersisted minted token"
        );
    }

    /// Proxy URL on a closed port: inline enrichment fails fast instead of
    /// reaching outside the test.
    fn dead_proxy_url() -> String {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        format!("http://127.0.0.1:{port}")
    }

    #[cfg(any())] // Non-interactive vendor session minting was removed from Atelier startup.
    #[tokio::test]
    async fn mint_session_noninteractive_uses_external_provider() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AtelierComConfig {
            auth_provider_command: Some("printf '%s' xai-ext-token".to_string()),
            ..AtelierComConfig::default()
        };
        let mgr = Arc::new(
            AuthManager::new(dir.path(), cfg.clone()).with_proxy_base_url(&dead_proxy_url()),
        );

        let auth = mint_session_noninteractive(&mgr, &cfg).await;
        assert_eq!(auth.map(|a| a.key), Some("xai-ext-token".to_string()));
    }

    /// External-provider output is team-pinned before persist (parity with OIDC
    /// / device-code): a wrong-team token is rejected and nothing is written.
    #[tokio::test]
    async fn external_provider_rejects_wrong_team_and_persists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(
            AuthManager::new(dir.path(), pinned_cfg("team-good"))
                .with_proxy_base_url(&dead_proxy_url()),
        );
        let cmd = emit_command(&team_jwt("team-wrong"));

        assert!(
            run_external_auth_provider(&cmd, &mgr, false, None)
                .await
                .is_err(),
            "wrong-team external token must be rejected"
        );
        assert!(
            mgr.current_or_expired().is_none(),
            "rejected external login must persist nothing"
        );
        assert!(
            !dir.path().join("auth.json").exists(),
            "rejected external login must not write auth.json"
        );
    }

    /// A matching-team external token is accepted and persisted.
    #[tokio::test]
    async fn external_provider_accepts_matching_team() {
        let dir = tempfile::tempdir().unwrap();
        let jwt = team_jwt("team-good");
        let mgr = Arc::new(
            AuthManager::new(dir.path(), pinned_cfg("team-good"))
                .with_proxy_base_url(&dead_proxy_url()),
        );
        let cmd = emit_command(&jwt);

        let (auth, _) = run_external_auth_provider(&cmd, &mgr, false, None)
            .await
            .expect("matching-team external token must be accepted");
        assert_eq!(auth.key, jwt);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn external_reauth_without_prev_auth_enriches_inline() {
        // Regression: reauth clears the manager before the provider runs with
        // is_refresh=true; flags must then come from /user, not default empty.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().route(
            "/user",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "userId": "u-1",
                    "teamBlockedReasons": ["BLOCKED_REASON_NO_LOGS"],
                }))
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(
            AuthManager::new(dir.path(), AtelierComConfig::default())
                .with_proxy_base_url(&format!("http://127.0.0.1:{port}")),
        );
        assert!(mgr.current_or_expired().is_none(), "precondition: no auth");

        let command = emit_command("fresh-token");
        let (auth, _) = run_external_auth_provider(&command, &mgr, true, None)
            .await
            .unwrap();
        assert_eq!(auth.key, "fresh-token");
        assert!(auth.is_zdr_team(), "flags must come from /user fetch");
        assert_eq!(auth.user_id, "u-1");
    }

    #[tokio::test]
    async fn external_refresh_carries_profile_without_network() {
        // Carry path must not need /user: dead proxy port, flags from prev.
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(
            AuthManager::new(dir.path(), AtelierComConfig::default())
                .with_proxy_base_url(&dead_proxy_url()),
        );
        mgr.hot_swap(AtelierAuth {
            team_blocked_reasons: vec!["BLOCKED_REASON_NO_LOGS".into()],
            organization_id: Some("org-1".into()),
            ..oidc_session("old-token", None)
        });

        let command = emit_command("fresh-token");
        let (auth, _) = run_external_auth_provider(&command, &mgr, true, None)
            .await
            .unwrap();
        assert_eq!(auth.key, "fresh-token");
        assert!(auth.is_zdr_team(), "flags must carry from previous auth");
        assert_eq!(auth.user_id, "test-user");
        assert_eq!(auth.organization_id.as_deref(), Some("org-1"));
    }

    #[cfg(any())] // Vendor device login and its external-provider precedence were removed.
    #[tokio::test]
    async fn device_flow_still_runs_external_provider() {
        // Regression: with the device flow opted into (--device-auth), the
        // external auth provider must still run first. `run_cli_login`'s device
        // branch goes through `run_auth_flow_interactive`, so that path must
        // pick up the provider instead of starting an interactive device login.
        let dir = tempfile::tempdir().unwrap();
        let cfg = AtelierComConfig {
            auth_provider_command: Some("printf '%s' xai-ext-token".to_string()),
            // oauth2=Some, oidc=None → the device flow is available (opt-in).
            ..AtelierComConfig::default()
        };
        assert!(
            cli_should_use_device(&cfg, LoginTransportOverride::ForceDevice).await,
            "precondition: --device-auth resolves to the device flow"
        );
        let mgr = Arc::new(
            AuthManager::new(dir.path(), cfg.clone()).with_proxy_base_url(&dead_proxy_url()),
        );
        let (auth, did_auth) = run_auth_flow_interactive(
            &mgr,
            &cfg,
            None,
            None,
            None,
            LoginTransportOverride::ForceDevice,
        )
        .await
        .expect("external provider should satisfy login without device flow");
        assert_eq!(
            auth.key, "xai-ext-token",
            "external provider token must win"
        );
        assert!(did_auth);
    }

    #[test]
    fn login_transport_override_maps_to_cli_bool() {
        // `--oauth` → loopback, `--device-auth` → device, no flag → no override.
        assert_eq!(LoginTransportOverride::None.as_cli_bool(), None);
        assert_eq!(
            LoginTransportOverride::ForceLoopback.as_cli_bool(),
            Some(false)
        );
        assert_eq!(
            LoginTransportOverride::ForceDevice.as_cli_bool(),
            Some(true)
        );
        // Pre-resolved transports are honored upstream, never via the CLI tier, so
        // they must not present a CLI value (which would mis-label the source as cli).
        assert_eq!(
            LoginTransportOverride::Preresolved(true).as_cli_bool(),
            None
        );
        assert_eq!(
            LoginTransportOverride::Preresolved(false).as_cli_bool(),
            None
        );
    }

    #[tokio::test]
    async fn preresolved_bypasses_resolver_and_is_never_cli() {
        // Regression for the double-log / source=cli bug: the inner flow must
        // honor `Preresolved` WITHOUT re-running the resolver. Each case pins the
        // opposite env value, so a leak into the resolver would flip the result —
        // returning the carried value proves the early return (and no second log).
        {
            let _guard = EnvVarGuard::set("ATELIER_LOGIN_DEVICE_FLOW", "false");
            assert!(
                should_use_device_flow(LoginTransportOverride::Preresolved(true)).await,
                "Preresolved(true) honors device without re-resolving"
            );
        }
        {
            let _guard = EnvVarGuard::set("ATELIER_LOGIN_DEVICE_FLOW", "true");
            assert!(
                !should_use_device_flow(LoginTransportOverride::Preresolved(false)).await,
                "Preresolved(false) honors loopback without re-resolving"
            );
            assert!(
                should_use_device_flow(LoginTransportOverride::None).await,
                "the resolver path still honors env (sole resolution)"
            );
        }
        // Even if it reached the resolver it carries no CLI value. Legacy
        // remote values are ignored in the vendorless build.
        with_device_flow_env(None, || {
            assert_eq!(
                resolve_device_flow(LoginTransportOverride::Preresolved(true), None, Some(true))
                    .source,
                crate::agent::config::ConfigSource::Default,
                "Preresolved must never resolve as the cli tier"
            );
        });
    }

    #[test]
    fn from_flags_prefers_oauth_over_device() {
        // `--oauth` (loopback) wins if both are set — a defensive guard for the
        // ACP meta path (clap already blocks both flags on the CLI).
        assert_eq!(
            LoginTransportOverride::from_flags(true, true),
            LoginTransportOverride::ForceLoopback
        );
        assert_eq!(
            LoginTransportOverride::from_flags(true, false),
            LoginTransportOverride::ForceLoopback
        );
        assert_eq!(
            LoginTransportOverride::from_flags(false, true),
            LoginTransportOverride::ForceDevice
        );
        assert_eq!(
            LoginTransportOverride::from_flags(false, false),
            LoginTransportOverride::None
        );
    }

    #[tokio::test]
    async fn enterprise_oidc_never_uses_device_flow() {
        // oidc=Some, oauth2=None: `atelier login` must use loopback, not device —
        // even when --device-auth forces device (which would otherwise be true).
        // ForceDevice short-circuits the remote settings fetch, so this stays hermetic.
        let cfg = AtelierComConfig {
            oidc: Some(crate::auth::OidcAuthConfig {
                issuer: "https://idp.example".into(),
                client_id: "client".into(),
                scopes: vec!["openid".into()],
                audience: None,
            }),
            oauth2: None,
            ..AtelierComConfig::default()
        };
        assert!(
            !cli_should_use_device(&cfg, LoginTransportOverride::ForceDevice).await,
            "enterprise OIDC must stay on loopback"
        );
    }

    #[test]
    fn device_flow_precedence_cli_beats_env_config_remote() {
        // CLI flag wins over a *conflicting* env + config + remote feature flag.
        with_device_flow_env(Some(true), || {
            assert!(
                !resolve_device_flow(
                    LoginTransportOverride::ForceLoopback,
                    Some(true),
                    Some(true)
                )
                .value,
                "--oauth must force loopback even when env+config+remote say device"
            );
        });
        with_device_flow_env(Some(false), || {
            assert!(
                resolve_device_flow(
                    LoginTransportOverride::ForceDevice,
                    Some(false),
                    Some(false)
                )
                .value,
                "--device-auth must force device even when env+config+remote say loopback"
            );
        });
    }

    #[test]
    fn device_flow_precedence_env_beats_config() {
        // No CLI flag: env wins over a conflicting config.
        with_device_flow_env(Some(false), || {
            assert!(!resolve_device_flow(LoginTransportOverride::None, Some(true), None).value);
        });
        with_device_flow_env(Some(true), || {
            assert!(resolve_device_flow(LoginTransportOverride::None, Some(false), None).value);
        });
    }

    #[test]
    fn device_flow_env_beats_remote() {
        // env sits above the remote feature flag.
        with_device_flow_env(Some(false), || {
            assert!(
                !resolve_device_flow(LoginTransportOverride::None, None, Some(true)).value,
                "env=loopback must win over remote=device"
            );
        });
        with_device_flow_env(Some(true), || {
            assert!(
                resolve_device_flow(LoginTransportOverride::None, None, Some(false)).value,
                "env=device must win over remote=loopback"
            );
        });
    }

    #[test]
    fn device_flow_config_beats_remote() {
        // Local config sits above the remote feature flag (env unset so config decides).
        with_device_flow_env(None, || {
            assert!(
                !resolve_device_flow(LoginTransportOverride::None, Some(false), Some(true)).value,
                "config=loopback must win over remote=device"
            );
            assert!(
                resolve_device_flow(LoginTransportOverride::None, Some(true), Some(false)).value,
                "config=device must win over remote=loopback"
            );
        });
    }

    #[test]
    fn device_flow_precedence_config_then_default() {
        // No CLI flag, no env: config decides; absent everything → loopback.
        with_device_flow_env(None, || {
            assert!(!resolve_device_flow(LoginTransportOverride::None, Some(false), None).value);
            assert!(resolve_device_flow(LoginTransportOverride::None, Some(true), None).value);
            assert!(
                !resolve_device_flow(LoginTransportOverride::None, None, None).value,
                "default is loopback"
            );
        });
    }

    #[test]
    fn vendorless_device_flow_ignores_remote_feature_flags() {
        with_device_flow_env(None, || {
            assert!(
                !resolve_device_flow(LoginTransportOverride::None, None, Some(true)).value,
                "a legacy vendor flag must not enable device auth"
            );
        });
    }

    #[test]
    fn device_flow_legacy_remote_is_ignored_then_default() {
        // No CLI flag, no env, no config: legacy vendor flags cannot affect Atelier.
        with_device_flow_env(None, || {
            assert!(
                !resolve_device_flow(LoginTransportOverride::None, None, Some(true)).value,
                "legacy remote=device must not enable device auth"
            );
            assert!(
                !resolve_device_flow(LoginTransportOverride::None, None, Some(false)).value,
                "legacy remote=loopback is also ignored"
            );
            assert!(
                !resolve_device_flow(LoginTransportOverride::None, None, None).value,
                "the local default is loopback"
            );
        });
    }

    #[test]
    fn device_flow_records_deciding_tier() {
        // The resolver records which tier decided, so the rollout ramp can log it.
        use crate::agent::config::ConfigSource;
        with_device_flow_env(Some(false), || {
            assert_eq!(
                resolve_device_flow(LoginTransportOverride::ForceDevice, Some(false), None).source,
                ConfigSource::Cli,
                "an explicit CLI flag is reported as the cli tier"
            );
        });
        with_device_flow_env(Some(true), || {
            assert_eq!(
                resolve_device_flow(LoginTransportOverride::None, None, Some(false)).source,
                ConfigSource::Env
            );
        });
        with_device_flow_env(None, || {
            assert_eq!(
                resolve_device_flow(LoginTransportOverride::None, Some(true), Some(false)).source,
                ConfigSource::Config
            );
            assert_eq!(
                resolve_device_flow(LoginTransportOverride::None, None, Some(true)).source,
                ConfigSource::Default,
                "legacy remote values are ignored"
            );
            assert_eq!(
                resolve_device_flow(LoginTransportOverride::None, None, None).source,
                ConfigSource::Default
            );
        });
    }

    fn legacy_auth() -> AtelierAuth {
        AtelierAuth {
            key: "k".into(),
            auth_mode: AuthMode::WebLogin,
            create_time: Utc::now(),
            user_id: "u".into(),
            email: None,
            first_name: None,
            last_name: None,
            profile_image_asset_id: None,
            principal_type: None,
            principal_id: None,
            team_id: None,
            team_name: None,
            team_role: None,
            organization_id: None,
            organization_name: None,
            organization_role: None,
            user_blocked_reason: None,
            team_blocked_reasons: vec![],
            coding_data_retention_opt_out: false,
            has_atelier_code_access: None,
            refresh_token: None,
            expires_at: None,
            oidc_issuer: None,
            oidc_client_id: None,
        }
    }

    fn oidc_auth(issuer: &str) -> AtelierAuth {
        AtelierAuth {
            oidc_issuer: Some(issuer.into()),
            auth_mode: AuthMode::Oidc,
            ..legacy_auth()
        }
    }

    #[test]
    fn weblogin_cred_is_never_compatible() {
        let cfg = AtelierComConfig {
            oidc: Some(crate::auth::OidcAuthConfig {
                issuer: "https://idp.example".into(),
                client_id: "client".into(),
                scopes: vec!["openid".into()],
                audience: None,
            }),
            ..AtelierComConfig::default()
        };
        assert!(!is_cached_credential_compatible(&legacy_auth(), &cfg));
    }

    #[test]
    fn oidc_cred_with_matching_issuer_is_compatible() {
        let cfg = AtelierComConfig {
            oidc: Some(crate::auth::OidcAuthConfig {
                issuer: "https://idp.example".into(),
                client_id: "client".into(),
                scopes: vec!["openid".into()],
                audience: None,
            }),
            ..AtelierComConfig::default()
        };
        assert!(is_cached_credential_compatible(
            &oidc_auth("https://idp.example"),
            &cfg,
        ));
    }

    #[test]
    fn external_cred_compatibility_follows_issuer() {
        let cfg = AtelierComConfig {
            oidc: Some(crate::auth::OidcAuthConfig {
                issuer: "https://idp.example".into(),
                client_id: "client".into(),
                scopes: vec!["openid".into()],
                audience: None,
            }),
            ..AtelierComConfig::default()
        };

        // A first-party external credential (provider emitted the issuer) is
        // reused by interactive login like an OIDC session instead of
        // re-running the provider.
        assert!(is_cached_credential_compatible(
            &AtelierAuth {
                auth_mode: AuthMode::External,
                ..oidc_auth("https://idp.example")
            },
            &cfg,
        ));

        // Without an issuer (bare-token providers), external credentials stay
        // incompatible and interactive login starts fresh, as before.
        assert!(!is_cached_credential_compatible(
            &AtelierAuth {
                auth_mode: AuthMode::External,
                oidc_issuer: None,
                ..legacy_auth()
            },
            &cfg,
        ));
    }

    fn ensure_crypto_provider() {
        let _ = jsonwebtoken::crypto::rust_crypto::DEFAULT_PROVIDER.install_default();
    }

    fn team_jwt(principal_id: &str) -> String {
        ensure_crypto_provider();
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &serde_json::json!({
                "sub": "user-1",
                "principal_type": "Team",
                "principal_id": principal_id,
                "exp": 9999999999u64,
            }),
            &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap()
    }

    fn pinned_cfg(team: &str) -> AtelierComConfig {
        AtelierComConfig {
            force_login_team_uuid: Some(crate::auth::config::ForceLoginTeam::Single(team.into())),
            ..AtelierComConfig::default()
        }
    }

    /// Under a team pin, a cached session for a different team is not reused by
    /// interactive login — it falls through to a fresh, compliant login.
    #[test]
    fn cached_cred_with_wrong_team_is_incompatible() {
        let auth = AtelierAuth {
            key: team_jwt("team-wrong"),
            ..oidc_auth(TEST_OIDC_ISSUER)
        };
        assert!(!is_cached_credential_compatible(
            &auth,
            &pinned_cfg("team-good")
        ));
    }

    /// A cached session for the pinned team is reused normally.
    #[test]
    fn cached_cred_with_matching_team_is_compatible() {
        let auth = AtelierAuth {
            key: team_jwt("team-good"),
            ..oidc_auth(TEST_OIDC_ISSUER)
        };
        assert!(is_cached_credential_compatible(
            &auth,
            &pinned_cfg("team-good")
        ));
    }

    // ── run_auth_flow: expired path with disk token ─────────────────

    /// When in-memory token is expired but disk has a valid token,
    /// run_auth_flow should return the disk token without interactive login.
    #[tokio::test]
    async fn run_auth_flow_uses_valid_disk_token_when_expired() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AtelierComConfig::default();

        // Write a valid token to disk via a second AuthManager (simulates
        // a sibling process that already refreshed).
        let writer = Arc::new(
            AuthManager::new(dir.path(), cfg.clone()).with_proxy_base_url("http://127.0.0.1:1"),
        );
        let valid_disk = AtelierAuth {
            key: "fresh-token-from-disk".into(),
            auth_mode: AuthMode::Oidc,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            refresh_token: Some("new-rt".into()),
            oidc_issuer: Some(TEST_OIDC_ISSUER.into()),
            oidc_client_id: Some("client-1".into()),
            ..AtelierAuth::test_default()
        };
        writer.update(valid_disk).await.unwrap();

        // Primary manager: in-memory token is expired
        let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));
        let expired = AtelierAuth {
            key: "expired-access-token".into(),
            auth_mode: AuthMode::Oidc,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            refresh_token: Some("old-rt".into()),
            oidc_issuer: Some(TEST_OIDC_ISSUER.into()),
            oidc_client_id: Some("client-1".into()),
            ..AtelierAuth::test_default()
        };
        mgr.hot_swap(expired);
        assert!(mgr.is_expired());

        let (auth, is_new_login) = run_auth_flow(
            &mgr,
            &cfg,
            false, // not reauth
            None,
            None,
            None,
            LoginTransportOverride::None,
        )
        .await
        .unwrap();

        assert_eq!(auth.key, "fresh-token-from-disk");
        assert!(!is_new_login, "should not be a new login");
        // In-memory should be updated via hot_swap
        assert_eq!(mgr.current().unwrap().key, "fresh-token-from-disk");
    }

    /// When in-memory token is valid (not expired), run_auth_flow should
    /// return it directly without checking disk.
    #[tokio::test]
    async fn run_auth_flow_returns_cached_when_valid() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AtelierComConfig::default();
        let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));

        let valid = AtelierAuth {
            key: "still-valid".into(),
            auth_mode: AuthMode::Oidc,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            oidc_issuer: Some(TEST_OIDC_ISSUER.into()),
            oidc_client_id: Some("client-1".into()),
            ..AtelierAuth::test_default()
        };
        mgr.hot_swap(valid);

        let (auth, is_new_login) = run_auth_flow(
            &mgr,
            &cfg,
            false,
            None,
            None,
            None,
            LoginTransportOverride::None,
        )
        .await
        .unwrap();

        assert_eq!(auth.key, "still-valid");
        assert!(!is_new_login);
    }

    #[tokio::test]
    async fn run_auth_flow_defers_to_consumer_refresh_on_transient_failure() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = AtelierComConfig::default();

        let writer = Arc::new(
            AuthManager::new(dir.path(), cfg.clone()).with_proxy_base_url("http://127.0.0.1:1"),
        );
        let expired_with_rt = AtelierAuth {
            key: "expired-access-token".into(),
            auth_mode: AuthMode::Oidc,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            refresh_token: Some("valid-refresh-token".into()),
            oidc_issuer: Some(TEST_OIDC_ISSUER.into()),
            oidc_client_id: Some("client-1".into()),
            ..AtelierAuth::test_default()
        };
        writer.update(expired_with_rt.clone()).await.unwrap();

        let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));
        mgr.hot_swap(expired_with_rt);
        assert!(mgr.is_expired());
        mgr.set_refresher(std::sync::Arc::new(AlwaysTransientRefresher));

        let (auth, is_new_login) = run_auth_flow(
            &mgr,
            &cfg,
            false, // not reauth
            None,
            None,
            None,
            LoginTransportOverride::None,
        )
        .await
        .unwrap();

        assert_eq!(auth.key, "expired-access-token");
        assert!(auth.refresh_token.is_some());
        assert!(!is_new_login);
    }

    #[cfg(any())] // Vendor WebLogin/device fallback was removed; Atelier fails closed to Providers.
    #[tokio::test]
    async fn run_auth_flow_falls_through_when_no_refresh_token() {
        let dir = tempfile::tempdir().unwrap();
        // Point the OAuth2 issuer at a non-routable address so the OIDC
        // discovery fails immediately without opening a browser window.
        let mut cfg = AtelierComConfig::default();
        cfg.oauth2.as_mut().unwrap().issuer = "http://127.0.0.1:1".into();

        let writer = Arc::new(
            AuthManager::new(dir.path(), cfg.clone()).with_proxy_base_url("http://127.0.0.1:1"),
        );
        let expired_no_rt = AtelierAuth {
            key: "expired-legacy".into(),
            auth_mode: AuthMode::WebLogin,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
            refresh_token: None,
            ..AtelierAuth::test_default()
        };
        writer.update(expired_no_rt.clone()).await.unwrap();

        let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));
        mgr.hot_swap(expired_no_rt);
        assert!(mgr.is_expired());

        mgr.set_refresher(std::sync::Arc::new(AlwaysTransientRefresher));

        // Force device explicitly so the assertion doesn't depend on ambient
        // ATELIER_LOGIN_DEVICE_FLOW / the real config file (the CLI override
        // short-circuits the config read).
        let result = run_auth_flow(
            &mgr,
            &cfg,
            false,
            None,
            None,
            None,
            LoginTransportOverride::ForceDevice,
        )
        .await;

        let err = result.unwrap_err();
        // Device flow fall-through hits the device-code endpoint (not OIDC
        // discovery).
        assert!(
            err.to_string().contains("/oauth2/device/code"),
            "expected device-code request error (proves flow fell through to interactive login), got: {err}"
        );
    }

    #[test]
    fn extract_url_from_external_provider_stderr() {
        let extract = |input: &str| -> String {
            input
                .split_whitespace()
                .find(|w| w.starts_with("https://"))
                .map(|u| u.to_owned())
                .unwrap_or_else(|| input.to_owned())
        };

        // Preamble text with URL
        assert_eq!(
            extract(
                "Visit the following link to sign into Atelier: https://auth.example.com/login?code=abc"
            ),
            "https://auth.example.com/login?code=abc"
        );

        // Multi-line with URL on second line
        assert_eq!(
            extract("Please sign in below\nhttps://auth.example.com/sso"),
            "https://auth.example.com/sso"
        );

        // Just a bare URL
        assert_eq!(
            extract("https://auth.example.com/login"),
            "https://auth.example.com/login"
        );

        // No URL at all — fallback to full content
        assert_eq!(extract("some opaque output"), "some opaque output");
    }

    /// CLI `atelier login` passes `on_stderr=None`; stderr must be inherited so
    /// sign-in URLs appear in real time. Piped stderr with no reader deadlocks
    /// once the child writes past the pipe buffer (~64 KiB).
    #[tokio::test]
    async fn external_provider_cli_path_does_not_deadlock_on_large_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(
            AuthManager::new(dir.path(), AtelierComConfig::default())
                .with_proxy_base_url(&dead_proxy_url()),
        );
        let (auth, _) = run_external_auth_provider(large_stderr_command(), &mgr, false, None)
            .await
            .expect("CLI path must inherit stderr so large stderr does not deadlock");
        assert_eq!(auth.key, "token");
    }

    struct AlwaysTransientRefresher;

    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for AlwaysTransientRefresher {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::TransientFailure {
                message: "simulated network failure".into(),
            }
        }
    }

    /// Faithful reproduction of the cached-token bypass: the exact
    /// repro JWT (wrong team) cached in `auth.json` under a pin, driven through
    /// the same `AuthManager::new` + `auth()` engine `try_ensure_fresh_auth`
    /// uses. Must be rejected and cleared; fails on the pre-fix tree.
    #[tokio::test]
    async fn noninteractive_auth_rejects_wrong_team_cached_token() {
        // {"principal_id":"team-wrong","sub":"user-1"} — note: no principal_type.
        const REPRO_JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJwcmluY2lwYWxfaWQiOiJ0ZWFtLXdyb25nIiwic3ViIjoidXNlci0xIn0.Signature";

        let dir = tempfile::tempdir().unwrap();
        let cfg = AtelierComConfig {
            force_login_team_uuid: Some(crate::auth::config::ForceLoginTeam::AnyOf(vec![
                "team-good".into(),
            ])),
            ..AtelierComConfig::default()
        };

        // Persist the wrong-team session exactly as the repro's auth.json does.
        let mut store = crate::auth::model::AuthStore::new();
        store.insert(
            cfg.auth_scope(),
            AtelierAuth {
                key: REPRO_JWT.into(),
                auth_mode: AuthMode::Oidc,
                team_id: Some("team-wrong".into()),
                expires_at: chrono::DateTime::from_timestamp(9_999_999_999, 0),
                ..AtelierAuth::test_default()
            },
        );
        let auth_path = dir.path().join("auth.json");
        crate::auth::storage::write_auth_json(&auth_path, &store).unwrap();

        // Same engine as `try_ensure_fresh_auth`.
        let auth_manager = Arc::new(AuthManager::new(dir.path(), cfg.clone()));
        auth_manager.configure_refresher(cfg.auth_provider_command.clone());

        assert!(
            auth_manager.auth().await.is_err(),
            "non-interactive auth must reject the wrong-team cached token"
        );
        assert!(
            auth_manager.current().is_none(),
            "wrong-team token must not be usable via current()"
        );
        assert!(
            !auth_path.exists(),
            "wrong-team auth.json must be cleared, forcing a compliant re-login"
        );
    }
}
