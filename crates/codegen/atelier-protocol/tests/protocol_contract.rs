use std::sync::Mutex;

use async_trait::async_trait;
use atelier_protocol::{
    ATELIER_PROTOCOL_VERSION, AtelierRpcClient, EventId, Page, PageRequest, ProtocolInfo,
    RoleListResult, RpcClientError, RpcError, RpcRequest, RpcResponse, RpcTransport,
    RuntimeStatusResult, StructuredErrorData,
};
use serde_json::{Value, json};

fn contract_fixture() -> Value {
    serde_json::from_str(include_str!("../../../../sdk/fixtures/rpc-contract.json"))
        .expect("rpc-contract.json must be valid JSON")
}

#[test]
fn rust_contract_decodes_and_preserves_existing_role_and_runtime_fixtures() {
    let fixture = contract_fixture();

    let roles: RoleListResult = serde_json::from_value(fixture["roleListResult"].clone())
        .expect("Rust SDK must decode the TypeScript/C# role fixture");
    assert_eq!(roles.roles[0].role_id.as_str(), "main");
    assert_eq!(roles.roles[0].config.provider, "local");
    assert!(roles.roles[0].config.fast_mode);
    assert_eq!(roles.roles[0].config.payload["temperature"], json!(0.2));
    assert_eq!(
        serde_json::to_value(&roles).expect("serialize role result"),
        fixture["roleListResult"]
    );

    let statuses: RuntimeStatusResult =
        serde_json::from_value(fixture["runtimeStatusResult"].clone())
            .expect("Rust SDK must decode the TypeScript/C# runtime fixture");
    assert_eq!(statuses.protocol_version, ATELIER_PROTOCOL_VERSION);
    assert_eq!(statuses.statuses[0].state, "running");
    assert_eq!(statuses.statuses[0].retry_count, 1);
    assert_eq!(
        serde_json::to_value(&statuses).expect("serialize runtime result"),
        fixture["runtimeStatusResult"]
    );
}

#[test]
fn protocol_negotiation_uses_client_preference_and_fails_without_overlap() {
    let info = ProtocolInfo::new(
        ATELIER_PROTOCOL_VERSION,
        [ATELIER_PROTOCOL_VERSION, "1.5"],
        ["event_replay"],
        ["_atelier/protocol/info"],
    );

    assert_eq!(
        ProtocolInfo::negotiate(
            &["1.5".to_owned(), "2.0".to_owned()],
            &info.supported_versions,
        ),
        Some("1.5".to_owned())
    );
    assert_eq!(
        ProtocolInfo::negotiate(&["1.0".to_owned()], &info.supported_versions),
        None
    );
}

#[test]
fn sequenced_event_uses_camel_case_wire_names_and_numeric_event_ids() {
    let mut sequencer = atelier_protocol::EventSequencer::with_next_id(EventId::new(42));
    let event = sequencer.next(
        "session-1",
        Some("turn-3".to_owned()),
        "runtime.state_changed",
        1_700_000_000_000,
        json!({"state": "running"}),
    );

    let wire = serde_json::to_value(event).expect("serialize event");
    assert_eq!(wire["eventId"], 42);
    assert_eq!(wire["sessionId"], "session-1");
    assert_eq!(wire["turnId"], "turn-3");
    assert_eq!(wire["type"], "runtime.state_changed");
    assert_eq!(wire["timestamp"], 1_700_000_000_000_i64);
    assert!(wire.get("event_id").is_none());
    assert!(wire.get("session_id").is_none());
    assert_eq!(sequencer.next_id(), EventId::new(43));
}

#[test]
fn pagination_contract_round_trips_cursor_limit_and_next_cursor() {
    let request: PageRequest = serde_json::from_value(json!({
        "cursor": "event:40",
        "limit": 2
    }))
    .expect("decode page request");
    assert_eq!(request.cursor.as_deref(), Some("event:40"));
    assert_eq!(request.limit, Some(2));

    let page = Page::new(vec![json!({"eventId": 41}), json!({"eventId": 42})])
        .with_next_cursor("event:42");
    assert_eq!(
        serde_json::to_value(page).expect("serialize page"),
        json!({
            "items": [{"eventId": 41}, {"eventId": 42}],
            "nextCursor": "event:42",
            "hasMore": true
        })
    );
}

#[test]
fn structured_rpc_error_preserves_machine_readable_data() {
    let response: RpcResponse = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": 7,
        "error": {
            "code": -32004,
            "message": "model unavailable",
            "data": {
                "kind": "model_unavailable",
                "retryable": false,
                "provider": "allm",
                "model": "deepseek-v4-flash"
            }
        }
    }))
    .expect("decode structured JSON-RPC error");

    let error = response.error.expect("error response");
    assert_eq!(error.code, -32004);
    let data: StructuredErrorData =
        serde_json::from_value(error.data.unwrap()).expect("decode machine-readable error data");
    assert_eq!(data.kind, "model_unavailable");
    assert!(!data.retryable);
    assert_eq!(data.details, Value::Null);
    assert_eq!(data.extra["provider"], "allm");
    assert_eq!(data.extra["model"], "deepseek-v4-flash");
    assert_eq!(
        serde_json::to_value(data).expect("re-encode structured error data"),
        json!({
            "kind": "model_unavailable",
            "retryable": false,
            "details": null,
            "provider": "allm",
            "model": "deepseek-v4-flash"
        })
    );
}

#[test]
fn role_fast_mode_defaults_false_on_input_but_remains_required_in_output() {
    let config: atelier_protocol::RoleConfig = serde_json::from_value(json!({
        "provider": "local",
        "model": "coding-model",
        "effort": null,
        "payload": {}
    }))
    .expect("RoleConfig must match the provider API's defaulted fast_mode input");

    assert!(!config.fast_mode);
    assert_eq!(
        serde_json::to_value(config).expect("serialize RoleConfig"),
        json!({
            "provider": "local",
            "model": "coding-model",
            "effort": null,
            "fast_mode": false,
            "payload": {}
        })
    );
}

struct MockTransport {
    requests: Mutex<Vec<RpcRequest>>,
    responses: Mutex<Vec<RpcResponse>>,
}

struct RecordingTransport {
    requests: Mutex<Vec<RpcRequest>>,
}

#[async_trait]
impl RpcTransport for RecordingTransport {
    async fn send(&self, request: RpcRequest) -> Result<RpcResponse, String> {
        let id = request.id.clone();
        self.requests.lock().unwrap().push(request);
        Ok(RpcResponse {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(json!({
                "protocolVersion": ATELIER_PROTOCOL_VERSION,
                "statuses": []
            })),
            error: None,
        })
    }
}

#[async_trait]
impl RpcTransport for MockTransport {
    async fn send(&self, request: RpcRequest) -> Result<RpcResponse, String> {
        self.requests.lock().unwrap().push(request);
        Ok(self.responses.lock().unwrap().remove(0))
    }
}

#[tokio::test]
async fn rust_client_has_typed_protocol_roles_runtime_and_remote_errors() {
    let fixture = contract_fixture();
    let transport = MockTransport {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(vec![
            RpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: Some(1_i64.into()),
                result: Some(json!({
                    "protocolVersion": "2.0",
                    "supportedVersions": ["2.0"],
                    "negotiatedVersion": "2.0",
                    "capabilities": ["event_replay"],
                    "methods": ["_atelier/role/list"]
                })),
                error: None,
            },
            RpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: Some(2_i64.into()),
                result: Some(fixture["roleListResult"].clone()),
                error: None,
            },
            RpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: Some(3_i64.into()),
                result: Some(fixture["runtimeStatusResult"].clone()),
                error: None,
            },
            RpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: Some(4_i64.into()),
                result: None,
                error: Some(RpcError {
                    code: -32004,
                    message: "model unavailable".to_owned(),
                    data: Some(json!({
                    "kind": "model_unavailable",
                    "retryable": false
                    })),
                }),
            },
        ]),
    };
    let mut client = AtelierRpcClient::new(transport);

    let info = client
        .protocol_info(&["2.0".to_owned()])
        .await
        .expect("negotiate protocol");
    assert_eq!(info.negotiated_version.as_deref(), Some("2.0"));
    assert_eq!(client.roles().await.expect("roles").roles.len(), 1);
    assert_eq!(
        client
            .runtime_status(None)
            .await
            .expect("runtime status")
            .statuses[0]
            .request_id
            .as_deref(),
        Some("request-1")
    );

    let error = client
        .call_value("_atelier/model/get", None)
        .await
        .expect_err("remote error must be returned");
    match error {
        RpcClientError::Remote(remote) => {
            assert_eq!(remote.code, -32004);
            assert_eq!(remote.data.unwrap()["kind"], "model_unavailable");
        }
        other => panic!("unexpected client error: {other}"),
    }
}

#[tokio::test]
async fn rust_client_convenience_methods_match_shared_wire_contract() {
    let fixture = contract_fixture();
    let transport = RecordingTransport {
        requests: Mutex::new(Vec::new()),
    };
    let mut client = AtelierRpcClient::new(transport);

    client.context_current(None).await.unwrap();
    client.context_list(None).await.unwrap();
    client.context_get(None).await.unwrap();
    client.request_list(None).await.unwrap();
    client.request_get(None).await.unwrap();
    client.trace_get(None).await.unwrap();
    client.runtime_status(None).await.unwrap();
    client.runtime_doctor(None).await.unwrap();
    client.runtime_cancel(None).await.unwrap();
    client.runtime_retry(None).await.unwrap();
    client.runtime_recover(None).await.unwrap();
    client.runtime_tasks(None).await.unwrap();
    client.role_list(None).await.unwrap();
    client.role_get(None).await.unwrap();
    client.role_set(None).await.unwrap();
    client.role_test(None).await.unwrap();
    client.context_snapshot_create(None).await.unwrap();
    client.context_snapshot_get(None).await.unwrap();
    client.context_snapshot_delete(None).await.unwrap();
    client.agent_spawn_derived(None).await.unwrap();
    client.agent_spawn_parallel(None).await.unwrap();
    client.session_fork(None).await.unwrap();
    client.btw_ask(None).await.unwrap();
    client.btw_get(None).await.unwrap();
    client.btw_list(None).await.unwrap();
    client.btw_delete(None).await.unwrap();
    client.task_list(None).await.unwrap();
    client.task_get(None).await.unwrap();
    client.task_detach(None).await.unwrap();
    client.task_attach(None).await.unwrap();
    client.task_cancel(None).await.unwrap();
    client.task_subscribe(None).await.unwrap();
    client.model_get(None).await.unwrap();
    client.model_update_wire_api(None).await.unwrap();
    client.model_provider_override_list(None).await.unwrap();
    client.model_provider_override_set(None).await.unwrap();
    client.model_provider_override_delete(None).await.unwrap();
    client.model_provider_override_test(None).await.unwrap();

    let actual = client
        .transport()
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| request.method.clone())
        .collect::<Vec<_>>();
    let expected = fixture["convenienceMethods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["wire"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}
