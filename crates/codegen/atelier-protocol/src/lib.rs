//! Public, transport-neutral Rust SDK for Atelier's versioned RPC protocol.
//!
//! Wire contracts live here rather than in the TUI. The low-level JSON-RPC
//! envelope and transport boundary are reused from `atelier-acp-runtime`, while the
//! typed Atelier extension results remain independent of shell implementation
//! types.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use atelier_acp_runtime::ProtocolInfo;
pub use atelier_acp_runtime::{
    ATELIER_PROTOCOL_CAPABILITIES, ATELIER_PROTOCOL_VERSION, ATELIER_SUPPORTED_PROTOCOL_VERSIONS,
    DEFAULT_EVENT_REPLAY_CAPACITY, EventId, EventReplayBuffer, EventReplayError, EventSequencer,
    REDACTED_VALUE, ReplayError, RpcClientError, RpcError, RpcId, RpcRequest, RpcResponse,
    RpcTransport, SequencedEvent, VersionedProtocol, redact_payload, redact_text,
};

/// One of Atelier's fixed model-assignment roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleId {
    Main,
    Explore,
    Implement,
    Review,
    Test,
    Compact,
    Summary,
    Title,
}

impl RoleId {
    /// Stable role identifier used on the wire and in configuration.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Explore => "explore",
            Self::Implement => "implement",
            Self::Review => "review",
            Self::Test => "test",
            Self::Compact => "compact",
            Self::Summary => "summary",
            Self::Title => "title",
        }
    }
}

/// Provider/model assignment serialized by `_atelier/role/*` methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleConfig {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(default)]
    pub payload: Map<String, Value>,
}

/// One role entry returned by `_atelier/role/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleListEntry {
    pub role_id: RoleId,
    /// Newer runtimes expose this flag; omission remains compatible with the
    /// original TypeScript/C# fixture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
    pub config: RoleConfig,
}

/// Typed result returned by `_atelier/role/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleListResult {
    pub roles: Vec<RoleListEntry>,
}

/// Stable, forward-compatible runtime status exposed to SDK clients.
///
/// `state` and `role` intentionally remain strings. Clients must tolerate
/// states added by a newer runtime instead of depending on an internal enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub session_id: String,
    pub state: String,
    pub started_at_ms: u64,
    pub last_progress_at_ms: u64,
    pub request_id: Option<String>,
    pub turn_id: Option<String>,
    pub role: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub timeout_ms: Option<u64>,
    pub retry_count: u32,
    pub cancel_supported: bool,
    pub diagnostic_message: Option<String>,
}

/// Typed result returned by `_atelier/runtime/status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatusResult {
    pub protocol_version: String,
    pub statuses: Vec<RuntimeStatus>,
}

/// Cursor-based pagination request shared by list-style extension methods.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// Cursor-based result that remains usable by stdio and WebSocket clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>) -> Self {
        Self {
            items,
            next_cursor: None,
            has_more: false,
        }
    }

    pub fn with_next_cursor(mut self, cursor: impl Into<String>) -> Self {
        self.next_cursor = Some(cursor.into());
        self.has_more = true;
        self
    }
}

/// Machine-readable data carried inside a JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredErrorData {
    pub kind: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub details: Value,
    /// Forward-compatible extension fields supplied by newer runtimes.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Typed Rust client over a caller-provided stdio, WebSocket, or in-process
/// transport.
pub struct AtelierRpcClient<T> {
    inner: atelier_acp_runtime::AtelierRpcClient<T>,
}

macro_rules! value_rpc_methods {
    ($($name:ident => $wire_method:literal),+ $(,)?) => {
        $(
            pub async fn $name(
                &mut self,
                params: Option<Value>,
            ) -> Result<Value, RpcClientError> {
                self.call_value($wire_method, params).await
            }
        )+
    };
}

impl<T> AtelierRpcClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            inner: atelier_acp_runtime::AtelierRpcClient::new(transport),
        }
    }

    pub fn transport(&self) -> &T {
        self.inner.transport()
    }
}

impl<T: RpcTransport> AtelierRpcClient<T> {
    pub async fn call_value(
        &mut self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<Value, RpcClientError> {
        self.inner.call_value(method, params).await
    }

    pub async fn call<R: serde::de::DeserializeOwned>(
        &mut self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<R, RpcClientError> {
        self.inner.call(method, params).await
    }

    pub async fn protocol_info(
        &mut self,
        requested_versions: &[String],
    ) -> Result<ProtocolInfo, RpcClientError> {
        self.inner.protocol_info(requested_versions).await
    }

    pub async fn roles(&mut self) -> Result<RoleListResult, RpcClientError> {
        self.call("_atelier/role/list", None).await
    }

    pub async fn runtime_status(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<RuntimeStatusResult, RpcClientError> {
        let params = session_id.map(|session_id| serde_json::json!({ "sessionId": session_id }));
        self.call("_atelier/runtime/status", params).await
    }

    value_rpc_methods! {
        context_current => "_atelier/context/current",
        context_list => "_atelier/context/list",
        context_get => "_atelier/context/get",
        request_list => "_atelier/request/list",
        request_get => "_atelier/request/get",
        trace_get => "_atelier/trace/get",
        runtime_doctor => "_atelier/runtime/doctor",
        runtime_cancel => "_atelier/runtime/cancel",
        runtime_retry => "_atelier/runtime/retry",
        runtime_recover => "_atelier/runtime/recover",
        runtime_tasks => "_atelier/runtime/tasks",
        role_list => "_atelier/role/list",
        role_get => "_atelier/role/get",
        role_set => "_atelier/role/update",
        role_test => "_atelier/role/test",
        context_snapshot_create => "_atelier/context_snapshot/create",
        context_snapshot_get => "_atelier/context_snapshot/get",
        context_snapshot_delete => "_atelier/context_snapshot/delete",
        agent_spawn_derived => "_atelier/agent/spawn_derived",
        agent_spawn_parallel => "_atelier/agent/spawn_parallel",
        session_fork => "_atelier/session/fork",
        btw_ask => "_atelier/btw/ask",
        btw_get => "_atelier/btw/get",
        btw_list => "_atelier/btw/list",
        btw_delete => "_atelier/btw/delete",
        task_list => "_atelier/task/list",
        task_get => "_atelier/task/get",
        task_detach => "_atelier/task/detach",
        task_attach => "_atelier/task/attach",
        task_cancel => "_atelier/task/cancel",
        task_subscribe => "_atelier/task/subscribe",
        model_get => "_atelier/model/get",
        model_update_wire_api => "_atelier/model/update_wire_api",
        model_provider_override_list => "_atelier/model_provider_override/list",
        model_provider_override_set => "_atelier/model_provider_override/set",
        model_provider_override_delete => "_atelier/model_provider_override/delete",
        model_provider_override_test => "_atelier/model_provider_override/test",
    }
}
