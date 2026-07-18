use std::{collections::VecDeque, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// Value used when a request payload field may contain a secret.
pub const REDACTED_VALUE: &str = "[REDACTED]";

/// Current version of Atelier's extension protocol.
pub const ATELIER_PROTOCOL_VERSION: &str = "2.0";

/// Versions understood by this runtime, in preference order.
pub const ATELIER_SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[ATELIER_PROTOCOL_VERSION];

/// Capabilities exposed by the Atelier control plane.
pub const ATELIER_PROTOCOL_CAPABILITIES: &[&str] = &[
    "context_inspector",
    "request_trace",
    "runtime_status",
    "runtime_recovery",
    "event_replay",
    "role_assignment",
    "provider_management",
    "policy_engine",
    "sandbox_diagnostics",
    "typed_hooks",
];

const JSONRPC_VERSION: &str = "2.0";

/// Default number of events retained by [`EventReplayBuffer::default`].
pub const DEFAULT_EVENT_REPLAY_CAPACITY: usize = 256;

/// A protocol version and the capabilities understood by the peer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedProtocol {
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl VersionedProtocol {
    /// Construct a protocol descriptor while preserving capability order.
    pub fn new<I, C>(version: impl Into<String>, capabilities: I) -> Self
    where
        I: IntoIterator<Item = C>,
        C: Into<String>,
    {
        Self {
            version: version.into(),
            capabilities: capabilities.into_iter().map(Into::into).collect(),
        }
    }
}

/// Versioned control-plane metadata returned by `_atelier/protocol/info`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolInfo {
    pub protocol_version: String,
    #[serde(default)]
    pub supported_versions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negotiated_version: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub methods: Vec<String>,
}

impl ProtocolInfo {
    pub fn new<I, C, M>(
        protocol_version: impl Into<String>,
        supported_versions: I,
        capabilities: C,
        methods: M,
    ) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
        C: IntoIterator,
        C::Item: Into<String>,
        M: IntoIterator,
        M::Item: Into<String>,
    {
        let protocol_version = protocol_version.into();
        let mut supported_versions = supported_versions
            .into_iter()
            .map(Into::into)
            .collect::<Vec<String>>();
        if !supported_versions
            .iter()
            .any(|version| version == &protocol_version)
        {
            supported_versions.insert(0, protocol_version.clone());
        }

        Self {
            protocol_version,
            supported_versions,
            negotiated_version: None,
            capabilities: capabilities.into_iter().map(Into::into).collect(),
            methods: methods.into_iter().map(Into::into).collect(),
        }
    }

    pub fn with_negotiated_version(mut self, version: Option<String>) -> Self {
        self.negotiated_version = version;
        self
    }

    /// Negotiate the first version requested by the client that this runtime
    /// supports. The order of `requested` is authoritative.
    pub fn negotiate(requested: &[String], supported: &[String]) -> Option<String> {
        requested
            .iter()
            .find(|candidate| supported.iter().any(|version| version == *candidate))
            .cloned()
    }
}

/// JSON-RPC identifier used by the extension SDKs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    String(String),
    Number(i64),
}

impl From<String> for RpcId {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for RpcId {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

/// Minimal JSON-RPC request envelope for `_atelier/*` methods.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: Option<RpcId>,
    pub method: String,
    pub params: Option<Value>,
}

impl RpcRequest {
    pub fn new(id: impl Into<RpcId>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id: Some(id.into()),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RpcRequestWire {
    jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<RpcId>,
    method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

impl Serialize for RpcRequest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_jsonrpc_version(&self.jsonrpc).map_err(serde::ser::Error::custom)?;
        RpcRequestWire {
            jsonrpc: self.jsonrpc.clone(),
            id: self.id.clone(),
            method: self.method.clone(),
            params: self.params.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RpcRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RpcRequestWire::deserialize(deserializer)?;
        validate_jsonrpc_version(&wire.jsonrpc).map_err(serde::de::Error::custom)?;
        Ok(Self {
            jsonrpc: wire.jsonrpc,
            id: wire.id,
            method: wire.method,
            params: wire.params,
        })
    }
}

/// JSON-RPC error object shared by the SDKs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Minimal JSON-RPC response envelope for `_atelier/*` methods.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Option<RpcId>,
    pub result: Option<Value>,
    pub error: Option<RpcError>,
}

impl RpcResponse {
    pub fn is_success(&self) -> bool {
        self.result.is_some() && self.error.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct RpcResponseWire {
    jsonrpc: String,
    id: Option<RpcId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

impl<'de> Deserialize<'de> for RpcResponseWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let object = Map::<String, Value>::deserialize(deserializer)?;
        let jsonrpc = object
            .get("jsonrpc")
            .ok_or_else(|| serde::de::Error::missing_field("jsonrpc"))
            .and_then(|value| {
                String::deserialize(value.clone()).map_err(serde::de::Error::custom)
            })?;
        let id = object
            .get("id")
            .filter(|value| !value.is_null())
            .map(|value| RpcId::deserialize(value.clone()))
            .transpose()
            .map_err(serde::de::Error::custom)?;
        let result = object.get("result").cloned();
        let error = object
            .get("error")
            .map(|value| RpcError::deserialize(value.clone()))
            .transpose()
            .map_err(serde::de::Error::custom)?;

        Ok(Self {
            jsonrpc,
            id,
            result,
            error,
        })
    }
}

impl Serialize for RpcResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        validate_jsonrpc_version(&self.jsonrpc).map_err(serde::ser::Error::custom)?;
        validate_response_shape(self.result.is_some(), self.error.is_some())
            .map_err(serde::ser::Error::custom)?;
        RpcResponseWire {
            jsonrpc: self.jsonrpc.clone(),
            id: self.id.clone(),
            result: self.result.clone(),
            error: self.error.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RpcResponseWire::deserialize(deserializer)?;
        validate_jsonrpc_version(&wire.jsonrpc).map_err(serde::de::Error::custom)?;
        validate_response_shape(wire.result.is_some(), wire.error.is_some())
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            jsonrpc: wire.jsonrpc,
            id: wire.id,
            result: wire.result,
            error: wire.error,
        })
    }
}

fn jsonrpc_version() -> String {
    JSONRPC_VERSION.to_owned()
}

fn validate_jsonrpc_version(version: &str) -> Result<(), String> {
    if version == JSONRPC_VERSION {
        Ok(())
    } else {
        Err(format!(
            "unsupported JSON-RPC version {version:?}; expected {JSONRPC_VERSION:?}"
        ))
    }
}

fn validate_response_shape(result_present: bool, error_present: bool) -> Result<(), String> {
    match (result_present, error_present) {
        (true, false) | (false, true) => Ok(()),
        (false, false) => {
            Err("JSON-RPC response must contain exactly one of result or error".into())
        }
        (true, true) => Err("JSON-RPC response cannot contain both result and error".into()),
    }
}

/// Monotonic event sequence number.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct EventId(u64);

impl EventId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<u64> for EventId {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl PartialEq<u64> for EventId {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl PartialEq<EventId> for u64 {
    fn eq(&self, other: &EventId) -> bool {
        *self == other.0
    }
}

/// An event with a sequence number and the session context needed to replay it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub event_id: EventId,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// The wire field is `type`; `event_type` avoids the Rust keyword.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Unix epoch timestamp in milliseconds.
    pub timestamp: i64,
    #[serde(serialize_with = "serialize_redacted_payload")]
    pub payload: Value,
}

impl SequencedEvent {
    pub fn new(
        event_id: EventId,
        session_id: impl Into<String>,
        turn_id: Option<String>,
        event_type: impl Into<String>,
        timestamp: i64,
        payload: Value,
    ) -> Self {
        Self {
            event_id,
            session_id: session_id.into(),
            turn_id,
            event_type: event_type.into(),
            timestamp,
            payload: redact_payload(&payload),
        }
    }
}

/// Allocates event ids in strictly increasing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventSequencer {
    next_id: EventId,
}

impl Default for EventSequencer {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSequencer {
    /// Create a sequencer whose first emitted id is `1`.
    pub const fn new() -> Self {
        Self {
            next_id: EventId::new(1),
        }
    }

    /// Create a sequencer with an explicit next id.
    pub const fn with_next_id(next_id: EventId) -> Self {
        Self { next_id }
    }

    pub const fn next_id(&self) -> EventId {
        self.next_id
    }

    pub const fn last_issued(&self) -> Option<EventId> {
        if self.next_id.get() == 0 {
            None
        } else {
            Some(EventId::new(self.next_id.get() - 1))
        }
    }

    pub fn allocate(&mut self) -> EventId {
        let event_id = self.next_id;
        self.next_id = EventId::new(
            event_id
                .get()
                .checked_add(1)
                .expect("event id sequence exhausted"),
        );
        event_id
    }

    pub fn next(
        &mut self,
        session_id: impl Into<String>,
        turn_id: Option<String>,
        event_type: impl Into<String>,
        timestamp: i64,
        payload: Value,
    ) -> SequencedEvent {
        SequencedEvent::new(
            self.allocate(),
            session_id,
            turn_id,
            event_type,
            timestamp,
            payload,
        )
    }
}

/// Failure returned when a replay cursor predates the retained event history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    TooOld {
        requested: EventId,
        oldest_available: EventId,
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooOld {
                requested,
                oldest_available,
            } => write!(
                f,
                "event replay cursor {requested} is too old; oldest available event is {oldest_available}"
            ),
        }
    }
}

impl std::error::Error for ReplayError {}

/// Alias that makes the error name explicit at call sites.
pub type EventReplayError = ReplayError;

/// Bounded in-memory history of sequenced events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventReplayBuffer {
    capacity: usize,
    events: VecDeque<SequencedEvent>,
}

impl Default for EventReplayBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_REPLAY_CAPACITY)
    }
}

impl EventReplayBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            events: VecDeque::new(),
        }
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn oldest_id(&self) -> Option<EventId> {
        self.events.front().map(|event| event.event_id)
    }

    pub fn latest(&self) -> Option<&SequencedEvent> {
        self.events.back()
    }

    pub fn latest_id(&self) -> Option<EventId> {
        self.latest().map(|event| event.event_id)
    }

    /// Append an event if its id is newer than the retained tail.
    ///
    /// Returns `false` for a zero-capacity buffer or a duplicate/out-of-order
    /// event. Rejected events never displace retained history.
    pub fn append(&mut self, event: SequencedEvent) -> bool {
        if self.capacity == 0
            || self
                .latest_id()
                .is_some_and(|latest| event.event_id <= latest)
        {
            return false;
        }

        let mut event = event;
        event.payload = redact_payload(&event.payload);
        self.events.push_back(event);
        while self.events.len() > self.capacity {
            self.events.pop_front();
        }
        true
    }

    /// Return retained events strictly after `after`.
    ///
    /// A cursor below the oldest retained id returns [`ReplayError::TooOld`]
    /// because the missing history can no longer be reconstructed from this
    /// bounded buffer. The initial cursor `0` is accepted only while event `1`
    /// is still retained.
    pub fn replay_from(&self, after: EventId) -> Result<Vec<SequencedEvent>, ReplayError> {
        let Some(oldest) = self.oldest_id() else {
            return Ok(Vec::new());
        };

        let initial_cursor_is_available = after == EventId::new(0) && oldest == EventId::new(1);
        if after < oldest && !initial_cursor_is_available {
            return Err(ReplayError::TooOld {
                requested: after,
                oldest_available: oldest,
            });
        }

        Ok(self
            .events
            .iter()
            .filter(|event| event.event_id > after)
            .cloned()
            .collect())
    }
}

/// High-level runtime lifecycle state carried by control and diagnostic data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    #[default]
    PreparingContext,
    RunningHook,
    CheckingPolicy,
    WaitingForProvider,
    StreamingResponse,
    RunningTool,
    RunningSubagent,
    WaitingForPermission,
    Compacting,
    GeneratingSummary,
    GeneratingTitle,
    Persisting,
    Paused,
    Recovering,
    Completed,
    Failed,
}

impl RuntimeState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Secret-free summary of one context block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBlockSummary {
    pub name: String,
    pub source: String,
    pub tokens: u64,
    pub redacted: bool,
}

impl ContextBlockSummary {
    pub fn new(name: impl Into<String>, tokens: u64) -> Self {
        Self {
            name: name.into(),
            source: String::new(),
            tokens,
            redacted: false,
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn with_redaction(mut self, redacted: bool) -> Self {
        self.redacted = redacted;
        self
    }

    pub fn token_count(&self) -> u64 {
        self.tokens
    }
}

/// Secret-free context accounting snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub blocks: Vec<ContextBlockSummary>,
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u64>,
    #[serde(default)]
    pub compaction_count: u64,
}

impl ContextSnapshot {
    pub fn new(blocks: Vec<ContextBlockSummary>, total_tokens: u64) -> Self {
        Self {
            blocks,
            total_tokens,
            context_limit: None,
            compaction_count: 0,
        }
    }

    pub fn from_blocks(blocks: Vec<ContextBlockSummary>) -> Self {
        let total_tokens = blocks.iter().map(|block| block.tokens).sum();
        Self::new(blocks, total_tokens)
    }

    pub fn with_context_limit(mut self, context_limit: u64) -> Self {
        self.context_limit = Some(context_limit);
        self
    }

    pub fn with_compaction_count(mut self, compaction_count: u64) -> Self {
        self.compaction_count = compaction_count;
        self
    }

    pub fn remaining_tokens(&self) -> Option<u64> {
        self.context_limit
            .map(|limit| limit.saturating_sub(self.total_tokens))
    }
}

/// Secret-free request timing and provider attribution summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestTraceSummary {
    pub request_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub role: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default)]
    pub fast_mode: bool,
    #[serde(serialize_with = "serialize_redacted_payload")]
    pub payload: Value,
    pub context_tokens: u64,
    pub latency_ms: u64,
    #[serde(default)]
    pub retry_count: u32,
    pub status: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_redacted_optional_text"
    )]
    pub error_stage: Option<String>,
}

impl RequestTraceSummary {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: impl Into<String>,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        role: impl Into<String>,
        provider: impl Into<String>,
        model: impl Into<String>,
        effort: Option<String>,
        fast_mode: bool,
        payload: Value,
        context_tokens: u64,
        latency_ms: u64,
        retry_count: u32,
        status: impl Into<String>,
        error_stage: Option<String>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            session_id: session_id.into(),
            turn_id: turn_id.into(),
            role: role.into(),
            provider: provider.into(),
            model: model.into(),
            effort,
            fast_mode,
            payload: Self::redact_payload(&payload),
            context_tokens,
            latency_ms,
            retry_count,
            status: status.into(),
            error_stage: error_stage.map(|value| redact_text(&value)),
        }
    }

    /// Return a recursively redacted copy without modifying the input.
    pub fn redact_payload(payload: &Value) -> Value {
        redact_payload(payload)
    }

    /// Return this summary's payload in secret-safe form.
    pub fn redacted_payload(&self) -> Value {
        Self::redact_payload(&self.payload)
    }

    /// Redact the in-memory payload before handing the summary to another
    /// component. Serialization also applies the same redaction.
    pub fn redact(&mut self) {
        self.payload = Self::redact_payload(&self.payload);
        self.error_stage = self.error_stage.take().map(|value| redact_text(&value));
    }
}

/// Recursively redact values stored under secret-bearing JSON object keys.
pub fn redact_payload(payload: &Value) -> Value {
    match payload {
        Value::Object(object) => {
            let mut redacted = Map::with_capacity(object.len());
            for (key, value) in object {
                if is_sensitive_key(key) {
                    redacted.insert(key.clone(), Value::String(REDACTED_VALUE.to_owned()));
                } else {
                    redacted.insert(key.clone(), redact_payload(value));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(values) => Value::Array(values.iter().map(redact_payload).collect()),
        Value::String(value) => Value::String(redact_text(value)),
        value => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();

    normalized == "authorization"
        || normalized == "cookie"
        || normalized == "password"
        || normalized == "passphrase"
        || normalized == "secret"
        || normalized == "credential"
        || normalized == "credentials"
        || normalized.contains("apikey")
        || normalized.contains("privatekey")
        || normalized.contains("clientsecret")
        || normalized == "token"
        || normalized.ends_with("token")
}

fn serialize_redacted_payload<S>(payload: &Value, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    redact_payload(payload).serialize(serializer)
}

fn serialize_redacted_optional_text<S>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    value.as_deref().map(redact_text).serialize(serializer)
}

fn serialize_redacted_text<S>(value: &str, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    redact_text(value).serialize(serializer)
}

/// Redact secret-bearing values embedded in human-readable trace text.
///
/// Structured payloads are redacted by [`redact_payload`]. This companion
/// function protects free-form provider errors and diagnostic messages where
/// the secret is carried in a header-like string rather than a JSON object
/// key. The matcher is intentionally conservative about where a value starts,
/// but deliberately consumes the whole value when its boundary is ambiguous.
pub fn redact_text(text: &str) -> String {
    redact_sensitive_assignments(&redact_private_key_blocks(text))
}

fn redact_private_key_blocks(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;

    while let Some(relative_start) = lower[cursor..].find("-----begin ") {
        let start = cursor + relative_start;
        let Some(relative_private_marker) = lower[start..].find("private key-----") else {
            cursor = start + "-----begin ".len();
            continue;
        };
        let begin_end = start + relative_private_marker + "private key-----".len();
        let Some(relative_end_start) = lower[begin_end..].find("-----end ") else {
            cursor = begin_end;
            continue;
        };
        let end_start = begin_end + relative_end_start;
        let Some(relative_end_marker) = lower[end_start..].find("private key-----") else {
            cursor = end_start + "-----end ".len();
            continue;
        };
        let end = end_start + relative_end_marker + "private key-----".len();

        output.push_str(&text[cursor..start]);
        output.push_str(REDACTED_VALUE);
        cursor = end;
    }

    output.push_str(&text[cursor..]);
    output
}

const SENSITIVE_TEXT_LABELS: &[&str] = &[
    "authorization",
    "access-token",
    "access_token",
    "refresh-token",
    "refresh_token",
    "private-key",
    "private_key",
    "private key",
    "client-secret",
    "client_secret",
    "api-key",
    "api_key",
    "apikey",
    "cookie",
    "password",
    "passphrase",
    "credential",
    "token",
];

fn redact_sensitive_assignments(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;

    while cursor < text.len() {
        let Some((start, label)) = find_next_sensitive_label(&lower, cursor) else {
            output.push_str(&text[cursor..]);
            break;
        };
        let label_end = start + label.len();
        let mut value_start = label_end;
        while text[value_start..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            value_start += text[value_start..].chars().next().unwrap().len_utf8();
        }

        if matches!(text.as_bytes().get(value_start), Some(b'"') | Some(b'\'')) {
            value_start += 1;
            while text[value_start..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
            {
                value_start += text[value_start..].chars().next().unwrap().len_utf8();
            }
        }

        let has_separator = matches!(text.as_bytes().get(value_start), Some(b':') | Some(b'='));
        if !has_separator {
            output.push_str(&text[cursor..label_end]);
            cursor = label_end;
            continue;
        }

        value_start += 1;
        while text[value_start..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            value_start += text[value_start..].chars().next().unwrap().len_utf8();
        }

        let quote = match text.as_bytes().get(value_start) {
            Some(b'"') | Some(b'\'') => Some(text.as_bytes()[value_start] as char),
            _ => None,
        };
        let content_start = value_start + usize::from(quote.is_some());
        let content_end = if let Some(quote) = quote {
            text[content_start..]
                .find(quote)
                .map(|offset| content_start + offset)
                .unwrap_or(text.len())
        } else {
            let delimiters = if label == "cookie" {
                ",\r\n}"
            } else {
                ",;\r\n&]}"
            };
            text[content_start..]
                .find(|character| delimiters.contains(character))
                .map(|offset| content_start + offset)
                .unwrap_or(text.len())
        };
        let value_end = if quote.is_some() && content_end < text.len() {
            content_end + 1
        } else {
            content_end
        };

        output.push_str(&text[cursor..content_start]);
        output.push_str(REDACTED_VALUE);
        if quote.is_some() && content_end < text.len() {
            output.push_str(&text[content_end..value_end]);
        }
        cursor = value_end;
    }

    output
}

fn find_next_sensitive_label(lower: &str, from: usize) -> Option<(usize, &'static str)> {
    SENSITIVE_TEXT_LABELS
        .iter()
        .filter_map(|label| {
            lower[from..]
                .find(label)
                .map(|relative| (from + relative, *label))
        })
        .filter(|(start, label)| {
            let before = lower[..*start].chars().next_back();
            let after = lower[*start + label.len()..].chars().next();
            !before.is_some_and(is_text_word_char) && !after.is_some_and(is_text_word_char)
        })
        .min_by(|(left_start, left_label), (right_start, right_label)| {
            left_start
                .cmp(right_start)
                .then_with(|| right_label.len().cmp(&left_label.len()))
        })
}

fn is_text_word_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// A point-in-time view of runtime progress monitored by a watchdog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchdogSnapshot {
    pub state: RuntimeState,
    pub healthy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at: Option<i64>,
    pub stalled_for_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

impl WatchdogSnapshot {
    pub fn new(
        state: RuntimeState,
        healthy: bool,
        last_progress_at: i64,
        stalled_for_ms: u64,
    ) -> Self {
        Self {
            state,
            healthy,
            last_progress_at: Some(last_progress_at),
            stalled_for_ms,
            timeout_ms: None,
        }
    }

    pub fn without_progress_timestamp(
        state: RuntimeState,
        healthy: bool,
        stalled_for_ms: u64,
    ) -> Self {
        Self {
            state,
            healthy,
            last_progress_at: None,
            stalled_for_ms,
            timeout_ms: None,
        }
    }

    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }
}

/// A structured, local-only runtime diagnostic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDiagnostic {
    pub code: String,
    pub severity: String,
    #[serde(serialize_with = "serialize_redacted_text")]
    pub message: String,
    pub recoverable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, serialize_with = "serialize_redacted_payload")]
    pub details: Value,
}

impl RuntimeDiagnostic {
    pub fn new(
        code: impl Into<String>,
        severity: impl Into<String>,
        message: impl Into<String>,
        recoverable: bool,
    ) -> Self {
        Self {
            code: code.into(),
            severity: severity.into(),
            message: redact_text(&message.into()),
            recoverable,
            timestamp: None,
            session_id: None,
            turn_id: None,
            details: Value::Null,
        }
    }

    pub fn with_timestamp(mut self, timestamp: i64) -> Self {
        self.timestamp = Some(timestamp);
        self
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_turn(mut self, turn_id: impl Into<String>) -> Self {
        self.turn_id = Some(turn_id.into());
        self
    }

    pub fn redact(&mut self) {
        self.message = redact_text(&self.message);
        self.details = redact_payload(&self.details);
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = redact_payload(&details);
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn versioned_protocol_and_event_round_trip() {
        let protocol = VersionedProtocol::new("2.3", ["events", "diagnostics"]);
        let mut sequencer = EventSequencer::new();
        let event = sequencer.next(
            "session-1",
            Some("turn-1".to_owned()),
            "turn_started",
            1_700_000_000_000,
            json!({"safe": true}),
        );

        let wire = serde_json::to_value((&protocol, &event)).expect("serialize protocol and event");
        assert_eq!(wire[0]["version"], "2.3");
        assert_eq!(wire[0]["capabilities"], json!(["events", "diagnostics"]));
        assert_eq!(wire[1]["event_id"], 1);
        assert_eq!(wire[1]["type"], "turn_started");

        let protocol_back: VersionedProtocol =
            serde_json::from_value(wire[0].clone()).expect("deserialize protocol");
        let event_back: SequencedEvent =
            serde_json::from_value(wire[1].clone()).expect("deserialize event");
        assert_eq!(protocol_back, protocol);
        assert_eq!(event_back, event);
    }

    #[test]
    fn sequenced_event_redacts_payload_before_replay_or_serialization() {
        let event = SequencedEvent::new(
            EventId::new(1),
            "session-1",
            None,
            "runtime.failed",
            1_700_000_000_000,
            json!({
                "details": {
                    "Authorization": "Bearer event-secret",
                    "message": "token: event-token-secret"
                }
            }),
        );

        assert_eq!(event.payload["details"]["Authorization"], REDACTED_VALUE);
        let wire = serde_json::to_string(&event).expect("serialize event");
        assert!(!wire.contains("event-secret"));
        assert!(!wire.contains("event-token-secret"));
    }

    #[test]
    fn replay_buffer_redacts_struct_literal_events_before_retaining_them() {
        let event = SequencedEvent {
            event_id: EventId::new(1),
            session_id: "session-1".to_owned(),
            turn_id: None,
            event_type: "runtime.failed".to_owned(),
            timestamp: 1_700_000_000_000,
            payload: json!({"token": "buffer-token-secret"}),
        };
        let mut buffer = EventReplayBuffer::new(1);

        assert!(buffer.append(event));
        assert_eq!(buffer.latest().unwrap().payload["token"], REDACTED_VALUE);
    }

    #[test]
    fn protocol_info_negotiates_requested_versions_and_round_trips_rpc_envelopes() {
        let info = ProtocolInfo::new(
            ATELIER_PROTOCOL_VERSION,
            ATELIER_SUPPORTED_PROTOCOL_VERSIONS.iter().copied(),
            ["context_inspector"],
            ["_atelier/context/current"],
        );
        assert_eq!(
            ProtocolInfo::negotiate(
                &["1.0".to_owned(), "2.0".to_owned()],
                &info.supported_versions,
            ),
            Some("2.0".to_owned())
        );
        assert_eq!(
            ProtocolInfo::negotiate(&["1.0".to_owned()], &info.supported_versions),
            None
        );

        let request = RpcRequest::new(
            "request-1",
            "_atelier/context/current",
            Some(json!({
                "sessionId": "session-1",
            })),
        );
        let response = RpcResponse {
            jsonrpc: "2.0".to_owned(),
            id: request.id.clone(),
            result: Some(json!({"available": true})),
            error: None,
        };
        let request_wire = serde_json::to_value(&request).expect("serialize rpc request");
        let response_wire = serde_json::to_value(&response).expect("serialize rpc response");
        assert_eq!(request_wire["jsonrpc"], "2.0");
        assert_eq!(request_wire["method"], "_atelier/context/current");
        assert_eq!(response_wire["result"]["available"], true);
        assert!(response.is_success());
    }

    #[test]
    fn rpc_request_requires_json_rpc_2_0_on_wire() {
        let missing_version = json!({
            "id": "request-1",
            "method": "_atelier/context/current"
        });
        let wrong_version = json!({
            "jsonrpc": "1.0",
            "id": "request-1",
            "method": "_atelier/context/current"
        });

        assert!(serde_json::from_value::<RpcRequest>(missing_version).is_err());
        assert!(serde_json::from_value::<RpcRequest>(wrong_version).is_err());

        let invalid = RpcRequest {
            jsonrpc: "1.0".to_owned(),
            id: Some(RpcId::from("request-1")),
            method: "_atelier/context/current".to_owned(),
            params: None,
        };
        assert!(serde_json::to_value(invalid).is_err());
    }

    #[test]
    fn rpc_response_requires_exactly_one_result_or_error() {
        let neither = json!({
            "jsonrpc": "2.0",
            "id": "request-1"
        });
        let both = json!({
            "jsonrpc": "2.0",
            "id": "request-1",
            "result": null,
            "error": {"code": -1, "message": "failed"}
        });
        let wrong_version_wire = json!({
            "jsonrpc": "1.0",
            "id": "request-1",
            "result": {"ok": true}
        });

        assert!(serde_json::from_value::<RpcResponse>(neither).is_err());
        assert!(serde_json::from_value::<RpcResponse>(both).is_err());
        assert!(serde_json::from_value::<RpcResponse>(wrong_version_wire).is_err());

        let null_result = json!({
            "jsonrpc": "2.0",
            "id": "request-1",
            "result": null
        });
        let null_result_response: RpcResponse =
            serde_json::from_value(null_result).expect("null is a valid JSON-RPC result");
        assert!(null_result_response.is_success());
        assert_eq!(
            serde_json::to_value(null_result_response).expect("serialize null result")["result"],
            Value::Null
        );

        let invalid = RpcResponse {
            jsonrpc: "2.0".to_owned(),
            id: Some(RpcId::from("request-1")),
            result: None,
            error: None,
        };
        assert!(!invalid.is_success());
        assert!(serde_json::to_value(invalid).is_err());

        let wrong_version = RpcResponse {
            jsonrpc: "1.0".to_owned(),
            id: Some(RpcId::from("request-1")),
            result: Some(json!({"ok": true})),
            error: None,
        };
        assert!(serde_json::to_value(wrong_version).is_err());
    }

    #[test]
    fn protocol_info_advertises_current_version_when_supported_versions_omit_it() {
        let info = ProtocolInfo::new(
            ATELIER_PROTOCOL_VERSION,
            ["1.0"],
            ["context_inspector"],
            ["_atelier/context/current"],
        );

        assert!(
            info.supported_versions
                .iter()
                .any(|version| version == ATELIER_PROTOCOL_VERSION)
        );
        assert_eq!(
            ProtocolInfo::negotiate(
                &[ATELIER_PROTOCOL_VERSION.to_owned()],
                &info.supported_versions,
            ),
            Some(ATELIER_PROTOCOL_VERSION.to_owned())
        );
    }

    #[test]
    fn event_sequencer_assigns_strictly_increasing_ids() {
        let mut sequencer = EventSequencer::new();
        let first = sequencer.next("session", None, "first", 10, json!(null));
        let second = sequencer.next("session", None, "second", 11, json!(null));

        assert_eq!(first.event_id, EventId::new(1));
        assert_eq!(second.event_id, EventId::new(2));
        assert!(first.event_id < second.event_id);
    }

    #[test]
    fn replay_buffer_is_bounded_and_replays_after_cursor() {
        let mut sequencer = EventSequencer::new();
        let mut buffer = EventReplayBuffer::new(2);
        let first = sequencer.next("session", None, "first", 1, json!(1));
        let second = sequencer.next("session", None, "second", 2, json!(2));
        let third = sequencer.next("session", None, "third", 3, json!(3));

        assert!(buffer.append(first));
        assert!(buffer.append(second.clone()));
        assert!(buffer.append(third.clone()));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.latest(), Some(&third));
        assert_eq!(buffer.latest_id(), Some(EventId::new(3)));

        let replay = buffer
            .replay_from(EventId::new(2))
            .expect("cursor at the retained boundary is replayable");
        assert_eq!(replay, vec![third]);
    }

    #[test]
    fn replay_buffer_rejects_a_cursor_older_than_retained_history() {
        let mut sequencer = EventSequencer::new();
        let mut buffer = EventReplayBuffer::new(2);
        for event_type in ["first", "second", "third"] {
            assert!(buffer.append(sequencer.next("session", None, event_type, 1, json!({}))));
        }

        assert_eq!(
            buffer.replay_from(EventId::new(1)),
            Err(ReplayError::TooOld {
                requested: EventId::new(1),
                oldest_available: EventId::new(2),
            })
        );
    }

    #[test]
    fn request_trace_redacts_secret_payload_keys_recursively() {
        let payload = json!({
            "prompt": "keep metadata only",
            "authorization": "Bearer secret",
            "nested": {
                "api_key": "secret-key",
                "safe": "visible"
            },
            "items": [{"password": "secret-password"}]
        });

        let redacted = RequestTraceSummary::redact_payload(&payload);
        assert_eq!(redacted["prompt"], "keep metadata only");
        assert_eq!(redacted["authorization"], REDACTED_VALUE);
        assert_eq!(redacted["nested"]["api_key"], REDACTED_VALUE);
        assert_eq!(redacted["nested"]["safe"], "visible");
        assert_eq!(redacted["items"][0]["password"], REDACTED_VALUE);
        assert_eq!(payload["authorization"], "Bearer secret");
    }

    #[test]
    fn request_trace_serialization_redacts_a_struct_literal_payload() {
        let summary = RequestTraceSummary {
            request_id: "request-1".to_owned(),
            session_id: "session-1".to_owned(),
            turn_id: "turn-1".to_owned(),
            role: "main".to_owned(),
            provider: "local".to_owned(),
            model: "model-a".to_owned(),
            effort: Some("high".to_owned()),
            fast_mode: true,
            payload: json!({"api_key": "must-not-leak", "kind": "completion"}),
            context_tokens: 12,
            latency_ms: 80,
            retry_count: 1,
            status: "completed".to_owned(),
            error_stage: None,
        };

        let wire = serde_json::to_value(summary).expect("serialize request trace");
        assert_eq!(wire["payload"]["api_key"], REDACTED_VALUE);
        assert_eq!(wire["payload"]["kind"], "completion");
    }

    #[test]
    fn runtime_trace_redacts_sensitive_text_and_nested_details() {
        let private_key =
            "-----BEGIN PRIVATE KEY-----\\nprivate-key-material\\n-----END PRIVATE KEY-----";
        let diagnostic = RuntimeDiagnostic::new(
            "provider_failed",
            "error",
            "request failed: Authorization: Bearer bearer-secret",
            true,
        )
        .with_details(json!({
            "safe": "keep this diagnostic label",
            "Authorization": "Bearer authorization-secret",
            "Cookie": "session=cookie-secret",
            "api-key": "api-key-secret",
            "nested": {
                "token": "token-secret",
                "private_key": private_key,
                "raw_pem": private_key,
                "message": "api-key: inline-api-key-secret"
            }
        }));
        let trace = RequestTraceSummary {
            request_id: "request-1".to_owned(),
            session_id: "session-1".to_owned(),
            turn_id: "turn-1".to_owned(),
            role: "main".to_owned(),
            provider: "local".to_owned(),
            model: "model-a".to_owned(),
            effort: None,
            fast_mode: false,
            payload: json!({
                "error": "Cookie: session=trace-cookie-secret",
                "provider_error": "{\"Authorization\":\"Bearer json-error-secret\"}",
                "privateKey": private_key
            }),
            context_tokens: 1,
            latency_ms: 2,
            retry_count: 0,
            status: "failed".to_owned(),
            error_stage: Some("Provider error: token=trace-token-secret".to_owned()),
        };

        let diagnostic_wire = serde_json::to_value(&diagnostic).expect("serialize diagnostic");
        let trace_wire = serde_json::to_value(&trace).expect("serialize trace");
        let diagnostic_json = diagnostic_wire.to_string();
        let trace_json = trace_wire.to_string();

        for secret in [
            "bearer-secret",
            "authorization-secret",
            "cookie-secret",
            "api-key-secret",
            "token-secret",
            "private-key-material",
            "inline-api-key-secret",
            "trace-cookie-secret",
            "json-error-secret",
            "trace-token-secret",
        ] {
            assert!(
                !diagnostic_json.contains(secret),
                "diagnostic leaked {secret}"
            );
            assert!(!trace_json.contains(secret), "trace leaked {secret}");
        }
        assert_eq!(diagnostic_wire["details"]["Authorization"], REDACTED_VALUE);
        assert_eq!(
            diagnostic_wire["details"]["nested"]["token"],
            REDACTED_VALUE
        );
        assert_eq!(
            diagnostic_wire["details"]["nested"]["private_key"],
            REDACTED_VALUE
        );
        assert_eq!(
            diagnostic_wire["details"]["nested"]["raw_pem"],
            REDACTED_VALUE
        );
        assert_eq!(trace_wire["payload"]["privateKey"], REDACTED_VALUE);
    }

    #[test]
    fn protocol_snapshots_round_trip() {
        let context = ContextSnapshot::new(
            vec![
                ContextBlockSummary::new("system", 100),
                ContextBlockSummary::new("conversation", 250),
            ],
            350,
        );
        let watchdog = WatchdogSnapshot::new(RuntimeState::StreamingResponse, true, 1_000, 50);
        let diagnostic =
            RuntimeDiagnostic::new("worker_stalled", "warning", "worker made no progress", true);

        for value in [
            serde_json::to_value(RuntimeState::WaitingForPermission).expect("state serialize"),
            serde_json::to_value(&context).expect("context serialize"),
            serde_json::to_value(&watchdog).expect("watchdog serialize"),
            serde_json::to_value(&diagnostic).expect("diagnostic serialize"),
        ] {
            assert!(!value.is_null());
        }

        let context_back: ContextSnapshot =
            serde_json::from_value(serde_json::to_value(&context).unwrap()).unwrap();
        let watchdog_back: WatchdogSnapshot =
            serde_json::from_value(serde_json::to_value(&watchdog).unwrap()).unwrap();
        let diagnostic_back: RuntimeDiagnostic =
            serde_json::from_value(serde_json::to_value(&diagnostic).unwrap()).unwrap();
        assert_eq!(context_back, context);
        assert_eq!(watchdog_back, watchdog);
        assert_eq!(diagnostic_back, diagnostic);
    }

    #[test]
    fn runtime_state_has_the_control_plane_terminal_boundary() {
        assert!(!RuntimeState::Recovering.is_terminal());
        assert!(RuntimeState::Completed.is_terminal());
        assert!(RuntimeState::Failed.is_terminal());
        assert_eq!(
            serde_json::to_value(RuntimeState::WaitingForProvider).unwrap(),
            json!("waiting_for_provider")
        );
    }
}
