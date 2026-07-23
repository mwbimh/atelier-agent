//! Managed MCP credential resolution via cli-chat-proxy.
//!
//! For remote MCP servers where the user has completed OAuth enrollment,
//! this module resolves credentials at agent init (cached across sessions)
//! and proactively refreshes them before token expiry.
//!
//! Config-file/plugin merge layering (which reads shell's config system) lives
//! in shell's `session::managed_mcp`, which re-exports everything here.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agent_client_protocol as acp;
use chrono::{DateTime, Utc};

/// Agent-level cache for managed MCP configs.
///
/// Explicit tri-state prevents concurrent double-fetches:
/// only the first caller transitions `NotFetched → Fetching`;
/// subsequent callers see `Fetching` and wait for the in-flight fetch.
pub enum ManagedMcpCache {
    NotFetched,
    /// Legacy in-flight state retained for persisted/cache compatibility.
    Fetching,
    /// May be empty if no managed servers are configured for this user.
    Ready(Vec<ManagedMcpConfig>),
}

/// Consecutive failed reactive re-auth attempts before a managed server is
/// parked in a terminal needs-auth state. Once reached, the cooldown gate
/// refuses further attempts until a successful proactive fetch clears it.
const MAX_REACTIVE_REAUTH_ATTEMPTS: u32 = 3;

/// Defensive upper bound (seconds) on the exponential backoff between reactive
/// re-auth attempts. The terminal attempt cap (`2^3 = 8s`) means the reactive
/// path never reaches this ceiling today.
const REACTIVE_REAUTH_BACKOFF_CAP_SECS: u64 = 64;

/// Per-server cooldown for the reactive managed re-auth path: a genuinely
/// revoked connector keeps returning a bad token, so each failed attempt pushes
/// the next eligible instant out by capped exponential backoff and the server
/// goes terminal after `MAX_REACTIVE_REAUTH_ATTEMPTS`.
#[derive(Debug, Clone)]
struct ManagedReauthState {
    consecutive_failures: u32,
    next_allowed_at: DateTime<Utc>,
}

impl Default for ManagedReauthState {
    fn default() -> Self {
        // No backoff window yet — the first attempt is always eligible.
        Self {
            consecutive_failures: 0,
            next_allowed_at: DateTime::<Utc>::MIN_UTC,
        }
    }
}

impl ManagedReauthState {
    /// Terminal once the attempt cap is hit: no further reactive attempts until
    /// a successful fetch clears the entry.
    fn is_terminal(&self) -> bool {
        self.consecutive_failures >= MAX_REACTIVE_REAUTH_ATTEMPTS
    }

    /// Eligible when the backoff window has elapsed and the cap is not reached.
    fn is_eligible(&self, now: DateTime<Utc>) -> bool {
        !self.is_terminal() && now >= self.next_allowed_at
    }

    /// Bump the failure count and push the next eligible instant out by
    /// `min(2^failures, REACTIVE_REAUTH_BACKOFF_CAP_SECS)` seconds.
    fn record_failure(&mut self, now: DateTime<Utc>) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let backoff_secs = 2u64
            .saturating_pow(self.consecutive_failures)
            .min(REACTIVE_REAUTH_BACKOFF_CAP_SECS);
        self.next_allowed_at = now + chrono::Duration::seconds(backoff_secs as i64);
    }
}

/// Agent-level cache for managed MCP gateway tool catalogs.
pub enum GatewayToolCatalogCache {
    NotFetched,
    /// Fetch in progress for the recorded gateway tool epoch.
    Fetching(u64),
    /// May be empty if the user has no gateway-exposed tools.
    Ready(GatewayToolCatalog),
}

pub struct ManagedMcpState {
    pub cache: ManagedMcpCache,
    pub gateway_tools_active: bool,
    pub gateway_tool_epoch: u64,
    pub gateway_tool_cache: GatewayToolCatalogCache,
    pub gateway_tool_fetch_notify: Arc<tokio::sync::Notify>,
    /// Retained across gateway disable/cache invalidation so the on-disk
    /// MCP descriptor mirror can remove stale gateway connector directories when
    /// the current catalog is empty or absent.
    pub gateway_tool_connectors_seen: HashSet<String>,
    /// Per-server reactive re-auth cooldown, keyed by MCP server name: one
    /// backoff entry per connector. Coalescing of concurrent attempts is
    /// best-effort — the caller takes the mutex sequentially for the
    /// `reauth_allowed` check and the later `record_reauth_failure`, not across
    /// the network attempt in between, so two simultaneously in-flight tool
    /// calls can each record a failure and reach the cap in fewer real re-auth
    /// rounds. Acceptable because the terminal state is cleared by the next
    /// proactive `clear_reauth_cooldowns`.
    reauth_cooldown: HashMap<String, ManagedReauthState>,
}

impl Default for ManagedMcpState {
    fn default() -> Self {
        Self {
            cache: ManagedMcpCache::NotFetched,
            gateway_tools_active: false,
            gateway_tool_epoch: 0,
            gateway_tool_cache: GatewayToolCatalogCache::NotFetched,
            gateway_tool_fetch_notify: Arc::new(tokio::sync::Notify::new()),
            gateway_tool_connectors_seen: HashSet::new(),
            reauth_cooldown: HashMap::new(),
        }
    }
}

impl ManagedMcpState {
    /// True if a reactive re-auth attempt for `server` is permitted at `now`:
    /// no prior cooldown entry, or the backoff window elapsed and the terminal
    /// attempt cap is not reached.
    pub fn reauth_allowed(&self, server: &str, now: DateTime<Utc>) -> bool {
        self.reauth_cooldown
            .get(server)
            .is_none_or(|state| state.is_eligible(now))
    }

    /// True once `server` exhausted `MAX_REACTIVE_REAUTH_ATTEMPTS` — the
    /// terminal needs-auth state that holds until a proactive refresh clears the
    /// cooldown or a reactive re-auth succeeds.
    pub fn reauth_is_terminal(&self, server: &str) -> bool {
        self.reauth_cooldown
            .get(server)
            .is_some_and(ManagedReauthState::is_terminal)
    }

    /// Record a failed reactive re-auth for `server`: bump the failure count and
    /// extend the backoff window.
    pub fn record_reauth_failure(&mut self, server: &str, now: DateTime<Utc>) {
        self.reauth_cooldown
            .entry(server.to_string())
            .or_default()
            .record_failure(now);
    }

    /// Reset `server`'s cooldown after a successful reactive re-auth.
    pub fn record_reauth_success(&mut self, server: &str) {
        self.reauth_cooldown.remove(server);
    }

    /// Clear every server's reactive re-auth cooldown. Invoked only by the
    /// proactive background refresh after a fresh fetch, so a parked (terminal)
    /// connector re-authorized on atelier.invalid can retry. The reactive path must NOT
    /// trigger this: a still-rejected token would reset its own attempt cap each
    /// attempt and loop instead of going terminal.
    pub fn clear_reauth_cooldowns(&mut self) {
        self.reauth_cooldown.clear();
    }

    pub fn enable_gateway_tools(&mut self) -> u64 {
        if !self.gateway_tools_active {
            self.gateway_tool_epoch = self.gateway_tool_epoch.wrapping_add(1);
        }
        self.gateway_tools_active = true;
        self.gateway_tool_epoch
    }

    pub fn start_gateway_tool_fetch(&mut self) -> Option<u64> {
        if !self.gateway_tools_active {
            return None;
        }
        self.gateway_tool_cache = GatewayToolCatalogCache::Fetching(self.gateway_tool_epoch);
        Some(self.gateway_tool_epoch)
    }

    pub fn complete_gateway_tool_fetch(&mut self, epoch: u64, catalog: GatewayToolCatalog) -> bool {
        if !self.gateway_tools_active || self.gateway_tool_epoch != epoch {
            self.gateway_tool_fetch_notify.notify_waiters();
            return false;
        }
        self.gateway_tool_connectors_seen
            .extend(catalog.tools.iter().map(|tool| tool.connector_id.clone()));
        self.gateway_tool_cache = GatewayToolCatalogCache::Ready(catalog);
        self.gateway_tool_fetch_notify.notify_waiters();
        true
    }

    pub fn fail_gateway_tool_fetch(&mut self, epoch: u64) {
        if self.gateway_tools_active
            && self.gateway_tool_epoch == epoch
            && matches!(self.gateway_tool_cache, GatewayToolCatalogCache::Fetching(fetch_epoch) if fetch_epoch == epoch)
        {
            self.gateway_tool_cache = GatewayToolCatalogCache::NotFetched;
        }
        self.gateway_tool_fetch_notify.notify_waiters();
    }

    pub fn disable_gateway_tools(&mut self) {
        self.gateway_tools_active = false;
        self.gateway_tool_epoch = self.gateway_tool_epoch.wrapping_add(1);
        self.gateway_tool_cache = GatewayToolCatalogCache::NotFetched;
        self.gateway_tool_fetch_notify.notify_waiters();
    }
}

pub type ManagedMcpStateHandle = Arc<tokio::sync::Mutex<ManagedMcpState>>;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManagedMcpConfig {
    /// Human-readable connector name (e.g. "Slack", "Linear").
    #[serde(default)]
    pub name: String,
    pub endpoint: String,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    pub token_expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub scope_id: Option<String>,
    #[serde(default)]
    pub scope_name: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GatewayToolCatalog {
    #[serde(default)]
    pub tools: Vec<GatewayTool>,
    #[serde(default)]
    pub total_tools: u32,
    #[serde(default)]
    pub connectors_needing_reauth: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GatewayTool {
    pub connector_id: String,
    pub connector_name: String,
    pub tool_id: String,
    pub tool_name: String,
    pub call_id: String,
    pub description: String,
    pub json_schema: serde_json::Value,
}

impl GatewayTool {
    pub fn qualified_name(&self) -> String {
        format!("{}__{}", self.connector_id, self.tool_id)
    }
}

/// Invalidate all managed MCP caches so the next caller refetches both legacy
/// managed configs and gateway tools.
pub async fn invalidate_cache(handle: &ManagedMcpStateHandle) {
    let mut state = handle.lock().await;
    state.cache = ManagedMcpCache::NotFetched;
    state.gateway_tool_cache = GatewayToolCatalogCache::NotFetched;
}

/// Invalidate only the gateway tool catalog so the next gateway-aware caller
/// refetches `/v1/mcp/tools/list`.
pub async fn invalidate_gateway_tool_cache(handle: &ManagedMcpStateHandle) {
    let mut state = handle.lock().await;
    state.gateway_tool_cache = GatewayToolCatalogCache::NotFetched;
}

/// Namespace prefix for managed MCP servers.
///
/// Servers with names starting with this prefix are managed by atelier.invalid —
/// their OAuth credentials are stored server-side.
/// Servers without this prefix are user-managed (local keychain, config.toml headers, etc.).
///
/// Examples:
///   `atelier_com_linear`  → managed by atelier.invalid
///   `atelier_com_slack`   → managed by atelier.invalid
///   `my_company_api`   → user-managed (local)
///
/// Single source of truth lives in `atelier-workspace` (which matches policy
/// `serverName`s against it); re-exported here so the two never drift.
pub use atelier_workspace::permission::resolution::MANAGED_MCP_PREFIX;

/// `"Linear"` -> `"atelier_com_linear"`. Shares normalization and the
/// `MANAGED_MCP_NAME_MAX_CHARS` cap with policy matching (`mcp_name_matches`) so
/// the runtime name and a policy `serverName` never drift.
pub fn to_managed_name(display_name: &str) -> String {
    use atelier_workspace::permission::resolution::{
        MANAGED_MCP_NAME_MAX_CHARS, normalize_managed_name,
    };
    let raw = format!(
        "{MANAGED_MCP_PREFIX}{}",
        normalize_managed_name(display_name)
    );
    atelier_shell_base::util::truncate(&raw, MANAGED_MCP_NAME_MAX_CHARS).to_string()
}

/// Returns `true` if this server should use server-side managed credentials.
///
/// Both conditions must hold:
/// 1. Server name starts with `MANAGED_MCP_PREFIX` ("atelier_com_")
/// 2. Server URL matches a managed config endpoint
///
/// This prevents false injection if a user accidentally names a server `atelier_com_*`
/// but it's not actually in the catalog, and prevents injecting into servers
/// that happen to share a URL but aren't opted in to managed auth.
pub fn should_inject_managed_auth(
    server_name: &str,
    server_url: &str,
    managed_by_url: &HashMap<String, &ManagedMcpConfig>,
) -> bool {
    server_name.starts_with(MANAGED_MCP_PREFIX)
        && managed_by_url.contains_key(&normalize_url(server_url))
}

pub fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

/// Key for managed config lookup: (normalized_url, scope, scope_id).
type ManagedConfigKey = (String, Option<String>, Option<String>);

/// Inject managed OAuth headers into `atelier_com_`-prefixed MCP servers.
///
/// Matches on endpoint URL + scope from `X-Connector-Scope` headers.
/// Falls back to URL-only match when no scope headers are present (backward compat).
/// Existing headers are preserved; managed headers are appended.
/// Non-prefixed servers are left untouched.
pub fn inject_managed_headers(servers: &mut [acp::McpServer], managed: &[ManagedMcpConfig]) {
    tracing::debug!(
        servers = servers.len(),
        managed = managed.len(),
        "Injecting managed MCP credentials"
    );
    if managed.is_empty() {
        return;
    }

    let managed_by_key: HashMap<ManagedConfigKey, &ManagedMcpConfig> = managed
        .iter()
        .map(|m| {
            let key = (
                normalize_url(&m.endpoint),
                m.scope.clone(),
                m.scope_id.clone(),
            );
            (key, m)
        })
        .collect();

    // URL-only fallback for backward compat
    let managed_by_url: HashMap<String, &ManagedMcpConfig> = managed
        .iter()
        .map(|m| (normalize_url(&m.endpoint), m))
        .collect();

    let mut injected = 0usize;
    let mut skipped_no_prefix = 0usize;
    let mut skipped_no_match = 0usize;
    let mut skipped_no_headers = 0usize;

    for server in servers.iter_mut() {
        let (name, url, headers) = match server {
            acp::McpServer::Http(acp::McpServerHttp {
                name, url, headers, ..
            })
            | acp::McpServer::Sse(acp::McpServerSse {
                name, url, headers, ..
            }) => (name.as_str(), url.as_str(), headers),
            _ => continue,
        };

        let normalized_url = normalize_url(url);

        if !name.starts_with(MANAGED_MCP_PREFIX) {
            if managed_by_url.contains_key(&normalized_url) {
                skipped_no_prefix += 1;
                tracing::debug!(
                    server_name = %name,
                    server_url = %url,
                    "Skipping managed injection: URL matches but name lacks '{}' prefix",
                    MANAGED_MCP_PREFIX,
                );
            }
            continue;
        }

        let scope = headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("x-connector-scope"))
            .map(|h| h.value.clone());
        let scope_id = headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("x-connector-scope-id"))
            .map(|h| h.value.clone());

        let config = match (&scope, &scope_id) {
            (Some(s), Some(id)) => {
                let key = (normalized_url.clone(), Some(s.clone()), Some(id.clone()));
                managed_by_key.get(&key).copied()
            }
            _ => managed_by_url.get(&normalized_url).copied(),
        };

        let Some(config) = config else {
            skipped_no_match += 1;
            tracing::debug!(
                server_name = %name,
                server_url = %url,
                scope = ?scope,
                scope_id = ?scope_id,
                "Skipping managed injection: no matching managed config"
            );
            continue;
        };

        if config.headers.is_empty() {
            skipped_no_headers += 1;
            tracing::debug!(
                server_name = %name,
                server_url = %url,
                "Skipping managed injection: managed config matched but has no headers",
            );
            continue;
        }

        let managed_keys: std::collections::HashSet<&str> =
            config.headers.keys().map(|k| k.as_str()).collect();
        headers.retain(|h| {
            !managed_keys.contains(h.name.as_str())
                && !h.name.eq_ignore_ascii_case("x-connector-scope")
                && !h.name.eq_ignore_ascii_case("x-connector-scope-id")
        });
        headers.extend(
            config
                .headers
                .iter()
                .map(|(k, v)| acp::HttpHeader::new(k.clone(), v.clone())),
        );

        injected += 1;
    }

    if injected > 0 {
        tracing::info!(count = injected, "Injected managed MCP credentials");
    }
    if skipped_no_prefix > 0 {
        tracing::info!(
            count = skipped_no_prefix,
            "Skipped servers with matching URLs but missing '{}' prefix",
            MANAGED_MCP_PREFIX,
        );
    }
    if skipped_no_match > 0 {
        tracing::info!(
            count = skipped_no_match,
            "Skipped servers with '{}' prefix but no matching managed config (URL+scope)",
            MANAGED_MCP_PREFIX,
        );
    }
    if skipped_no_headers > 0 {
        tracing::info!(
            count = skipped_no_headers,
            "Skipped servers with matching managed config but empty headers",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_managed(name: &str, endpoint: &str, scope: &str) -> ManagedMcpConfig {
        ManagedMcpConfig {
            name: name.to_string(),
            endpoint: endpoint.to_string(),
            headers: HashMap::from([("Authorization".into(), "Bearer tok".into())]),
            token_expires_at: None,
            scope: Some(scope.to_string()),
            scope_id: Some(format!("{scope}-id-123")),
            scope_name: None,
        }
    }

    #[test]
    fn gateway_tool_catalog_deserializes() {
        let catalog: GatewayToolCatalog = serde_json::from_str(
            r#"{
            "tools": [
                {
                    "connector_id": "gmail",
                    "connector_name": "Gmail",
                    "tool_id": "search",
                    "tool_name": "Search Gmail",
                    "call_id": "gmail_search",
                    "description": "Search email by query",
                    "json_schema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" }
                        },
                        "required": ["query"]
                    }
                }
            ],
            "total_tools": 1,
            "connectors_needing_reauth": ["Slack"]
        }"#,
        )
        .unwrap();

        assert_eq!(1, catalog.total_tools);
        let without_total_tools: GatewayToolCatalog = serde_json::from_str(
            r#"{
            "tools": [],
            "connectors_needing_reauth": []
        }"#,
        )
        .unwrap();
        assert_eq!(0, without_total_tools.total_tools);
        assert_eq!(vec!["Slack"], catalog.connectors_needing_reauth);
        assert_eq!("gmail_search", catalog.tools[0].call_id);
        assert_eq!("gmail__search", catalog.tools[0].qualified_name());
        assert_eq!("gmail", catalog.tools[0].connector_id);
        assert_eq!("Gmail", catalog.tools[0].connector_name);
        assert_eq!("search", catalog.tools[0].tool_id);
        assert_eq!("Search Gmail", catalog.tools[0].tool_name);
        assert_eq!(
            Some("string"),
            catalog.tools[0]
                .json_schema
                .pointer("/properties/query/type")
                .and_then(|v| v.as_str())
        );
    }

    #[test]
    fn disable_gateway_tools_clears_cached_catalog() {
        let mut state = ManagedMcpState::default();
        state.enable_gateway_tools();
        let epoch = state.start_gateway_tool_fetch().unwrap();
        assert!(state.complete_gateway_tool_fetch(
            epoch,
            GatewayToolCatalog {
                tools: vec![],
                total_tools: 0,
                connectors_needing_reauth: vec![],
            }
        ));
        assert!(state.gateway_tools_active);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::Ready(_)
        ));

        state.disable_gateway_tools();
        assert!(!state.gateway_tools_active);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::NotFetched
        ));
    }

    #[test]
    fn stale_gateway_tool_fetch_success_does_not_commit_after_disable() {
        let mut state = ManagedMcpState::default();
        state.enable_gateway_tools();
        let epoch = state.start_gateway_tool_fetch().unwrap();
        state.disable_gateway_tools();

        let committed = state.complete_gateway_tool_fetch(
            epoch,
            GatewayToolCatalog {
                tools: vec![],
                total_tools: 0,
                connectors_needing_reauth: vec![],
            },
        );

        assert!(!committed);
        assert!(!state.gateway_tools_active);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::NotFetched
        ));
    }

    #[test]
    fn failed_gateway_tool_fetch_does_not_clear_ready_catalog_from_same_epoch() {
        let mut state = ManagedMcpState::default();
        state.enable_gateway_tools();
        let epoch = state.start_gateway_tool_fetch().unwrap();
        assert!(state.complete_gateway_tool_fetch(
            epoch,
            GatewayToolCatalog {
                tools: vec![],
                total_tools: 0,
                connectors_needing_reauth: vec![],
            },
        ));

        state.fail_gateway_tool_fetch(epoch);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::Ready(_)
        ));
    }

    #[tokio::test]
    async fn gateway_tool_fetch_waiter_survives_notify_before_await() {
        let handle = ManagedMcpStateHandle::default();
        let (epoch, registered) = {
            let mut state = handle.lock().await;
            state.enable_gateway_tools();
            let epoch = state.start_gateway_tool_fetch().unwrap();
            (
                epoch,
                state.gateway_tool_fetch_notify.clone().notified_owned(),
            )
        };
        handle.lock().await.fail_gateway_tool_fetch(epoch);
        tokio::time::timeout(std::time::Duration::from_secs(1), registered)
            .await
            .expect("registered gateway catalog waiter must observe notify_waiters");
    }

    /// A fresh server with no cooldown entry is always eligible, and its first
    /// failure escalates the backoff window without going terminal yet.
    #[test]
    fn reauth_first_attempt_allowed_then_backs_off() {
        let mut state = ManagedMcpState::default();
        let now = Utc::now();
        assert!(state.reauth_allowed("atelier_com_slack", now));
        assert!(!state.reauth_is_terminal("atelier_com_slack"));

        state.record_reauth_failure("atelier_com_slack", now);
        // 2^1 = 2s backoff: not eligible now, eligible after the window.
        assert!(!state.reauth_allowed("atelier_com_slack", now));
        assert!(state.reauth_allowed("atelier_com_slack", now + chrono::Duration::seconds(2)));
        assert!(!state.reauth_is_terminal("atelier_com_slack"));
    }

    /// Backoff escalates per failure and is capped at
    /// `REACTIVE_REAUTH_BACKOFF_CAP_SECS`; the cap is observed by forcing the
    /// failure count high enough that `2^n` would exceed it.
    #[test]
    fn reauth_backoff_escalates_and_caps() {
        let mut state = ManagedReauthState::default();
        let now = Utc::now();

        state.record_failure(now);
        assert_eq!(state.next_allowed_at, now + chrono::Duration::seconds(2));
        state.record_failure(now);
        assert_eq!(state.next_allowed_at, now + chrono::Duration::seconds(4));

        // Drive failures past the cap exponent; the window must clamp to 64s.
        for _ in 0..10 {
            state.record_failure(now);
        }
        assert_eq!(
            state.next_allowed_at,
            now + chrono::Duration::seconds(REACTIVE_REAUTH_BACKOFF_CAP_SECS as i64)
        );
    }

    /// After `MAX_REACTIVE_REAUTH_ATTEMPTS` consecutive failures the server is
    /// terminal and never eligible again — even past the backoff window — until
    /// the cooldown is cleared.
    #[test]
    fn reauth_goes_terminal_after_max_attempts() {
        let mut state = ManagedMcpState::default();
        let now = Utc::now();
        for _ in 0..MAX_REACTIVE_REAUTH_ATTEMPTS {
            state.record_reauth_failure("atelier_com_slack", now);
        }
        assert!(state.reauth_is_terminal("atelier_com_slack"));
        // Even far past any backoff window, a terminal server stays ineligible.
        assert!(!state.reauth_allowed("atelier_com_slack", now + chrono::Duration::seconds(3600)));
    }

    /// A successful reactive re-auth resets that server's cooldown so the next
    /// failure starts a fresh backoff.
    #[test]
    fn reauth_success_resets_cooldown() {
        let mut state = ManagedMcpState::default();
        let now = Utc::now();
        for _ in 0..MAX_REACTIVE_REAUTH_ATTEMPTS {
            state.record_reauth_failure("atelier_com_slack", now);
        }
        assert!(state.reauth_is_terminal("atelier_com_slack"));

        state.record_reauth_success("atelier_com_slack");
        assert!(state.reauth_allowed("atelier_com_slack", now));
        assert!(!state.reauth_is_terminal("atelier_com_slack"));
    }
}
