use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt;

use crate::{ProtocolInfo, RpcError, RpcId, RpcRequest, RpcResponse};

/// Transport-neutral async boundary used by Atelier SDK clients.
///
/// stdio, WebSocket, and in-process ACP bridges can implement this trait
/// without making the protocol crate depend on a particular transport.
#[async_trait]
pub trait RpcTransport: Send + Sync {
    async fn send(&self, request: RpcRequest) -> Result<RpcResponse, String>;
}

#[derive(Debug)]
pub enum RpcClientError {
    Transport(String),
    InvalidResponse(String),
    Remote(RpcError),
    Decode(String),
    IncompatibleProtocol {
        requested: Vec<String>,
        supported: Vec<String>,
    },
}

impl fmt::Display for RpcClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "RPC transport failed: {error}"),
            Self::InvalidResponse(error) => write!(formatter, "invalid RPC response: {error}"),
            Self::Remote(error) => write!(
                formatter,
                "remote RPC error {}: {}",
                error.code, error.message
            ),
            Self::Decode(error) => write!(formatter, "RPC response decode failed: {error}"),
            Self::IncompatibleProtocol {
                requested,
                supported,
            } => write!(
                formatter,
                "no compatible Atelier protocol version; requested={requested:?}, supported={supported:?}"
            ),
        }
    }
}

impl std::error::Error for RpcClientError {}

/// Minimal client for the versioned Atelier extension protocol.
pub struct AtelierRpcClient<T> {
    transport: T,
    next_id: i64,
}

impl<T> AtelierRpcClient<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_id: 1,
        }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl<T: RpcTransport> AtelierRpcClient<T> {
    pub async fn call_value(
        &mut self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<Value, RpcClientError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| RpcClientError::InvalidResponse("RPC request id exhausted".into()))?;
        let response = self
            .transport
            .send(RpcRequest::new(RpcId::Number(id), method, params))
            .await
            .map_err(RpcClientError::Transport)?;
        if response.jsonrpc != "2.0" {
            return Err(RpcClientError::InvalidResponse(
                "response JSON-RPC version is not 2.0".into(),
            ));
        }
        if let Some(error) = response.error {
            return Err(RpcClientError::Remote(error));
        }
        response
            .result
            .ok_or_else(|| RpcClientError::InvalidResponse("response has no result".into()))
    }

    pub async fn call<R: DeserializeOwned>(
        &mut self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> Result<R, RpcClientError> {
        let value = self.call_value(method, params).await?;
        serde_json::from_value(value).map_err(|error| RpcClientError::Decode(error.to_string()))
    }

    pub async fn protocol_info(
        &mut self,
        requested_versions: &[String],
    ) -> Result<ProtocolInfo, RpcClientError> {
        let params = (!requested_versions.is_empty())
            .then(|| serde_json::json!({ "supportedVersions": requested_versions }));
        let info: ProtocolInfo = self.call("_atelier/protocol/info", params).await?;
        if !requested_versions.is_empty()
            && ProtocolInfo::negotiate(requested_versions, &info.supported_versions).is_none()
        {
            return Err(RpcClientError::IncompatibleProtocol {
                requested: requested_versions.to_vec(),
                supported: info.supported_versions,
            });
        }
        Ok(info)
    }

    pub async fn runtime_status(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<Value, RpcClientError> {
        let params = session_id.map(|session_id| serde_json::json!({ "sessionId": session_id }));
        self.call_value("_atelier/runtime/status", params).await
    }

    pub async fn roles(&mut self) -> Result<Value, RpcClientError> {
        self.call_value("_atelier/role/list", None).await
    }

    pub async fn context_current(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<Value, RpcClientError> {
        let params = session_id.map(|session_id| serde_json::json!({ "sessionId": session_id }));
        self.call_value("_atelier/context/current", params).await
    }

    pub async fn request_list(
        &mut self,
        session_id: Option<&str>,
    ) -> Result<Value, RpcClientError> {
        let params = session_id.map(|session_id| serde_json::json!({ "sessionId": session_id }));
        self.call_value("_atelier/request/list", params).await
    }

    pub async fn trace_get(
        &mut self,
        session_id: Option<&str>,
        after_event_id: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Value, RpcClientError> {
        let mut params = serde_json::Map::new();
        if let Some(session_id) = session_id {
            params.insert("sessionId".into(), Value::String(session_id.into()));
        }
        if let Some(after_event_id) = after_event_id {
            params.insert("afterEventId".into(), serde_json::json!(after_event_id));
        }
        if let Some(limit) = limit {
            params.insert("limit".into(), serde_json::json!(limit));
        }
        let params = (!params.is_empty()).then_some(Value::Object(params));
        self.call_value("_atelier/trace/get", params).await
    }

    pub async fn recover(&mut self, session_id: &str) -> Result<Value, RpcClientError> {
        self.call_value(
            "_atelier/runtime/recover",
            Some(serde_json::json!({ "sessionId": session_id })),
        )
        .await
    }

    pub async fn retry(&mut self, request_id: &str) -> Result<Value, RpcClientError> {
        self.call_value(
            "_atelier/runtime/retry",
            Some(serde_json::json!({ "requestId": request_id })),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockTransport {
        requests: Mutex<Vec<RpcRequest>>,
        response: Mutex<Option<RpcResponse>>,
    }

    #[async_trait]
    impl RpcTransport for MockTransport {
        async fn send(&self, request: RpcRequest) -> Result<RpcResponse, String> {
            self.requests.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| "missing mock response".to_owned())
        }
    }

    #[tokio::test]
    async fn client_negotiates_protocol_and_builds_versioned_requests() {
        let transport = MockTransport {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(Some(RpcResponse {
                jsonrpc: "2.0".into(),
                id: Some("1".into()),
                result: Some(serde_json::json!({
                    "protocolVersion": "2.0",
                    "supportedVersions": ["2.0"],
                    "capabilities": [],
                    "methods": []
                })),
                error: None,
            })),
        };
        let mut client = AtelierRpcClient::new(transport);
        let info = client
            .protocol_info(&["2.0".to_owned()])
            .await
            .expect("protocol negotiation");
        assert_eq!(info.protocol_version, "2.0");
        assert_eq!(
            client.transport().requests.lock().unwrap()[0].method,
            "_atelier/protocol/info"
        );
    }

    #[tokio::test]
    async fn client_surfaces_remote_errors_without_fallback() {
        let transport = MockTransport {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(Some(RpcResponse {
                jsonrpc: "2.0".into(),
                id: Some("1".into()),
                result: None,
                error: Some(RpcError {
                    code: -32602,
                    message: "model unavailable".into(),
                    data: None,
                }),
            })),
        };
        let mut client = AtelierRpcClient::new(transport);
        let error = client.roles().await.expect_err("remote error must surface");
        assert!(error.to_string().contains("model unavailable"));
    }

    #[tokio::test]
    async fn client_builds_runtime_retry_request() {
        let transport = MockTransport {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(Some(RpcResponse {
                jsonrpc: "2.0".into(),
                id: Some("1".into()),
                result: Some(serde_json::json!({ "accepted": true })),
                error: None,
            })),
        };
        let mut client = AtelierRpcClient::new(transport);
        client
            .retry("request-1")
            .await
            .expect("retry request should succeed");

        let request = &client.transport().requests.lock().unwrap()[0];
        assert_eq!(request.method, "_atelier/runtime/retry");
        assert_eq!(
            request.params.as_ref().unwrap()["requestId"],
            serde_json::json!("request-1")
        );
    }
}
