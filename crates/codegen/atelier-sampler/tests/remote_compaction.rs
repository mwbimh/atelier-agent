use atelier_sampler::{
    ApiBackend, AuthScheme, BearerResolver, RemoteCompactionV2Client, SamplerConfig, SamplingError,
};
use atelier_sampling_types::{ConversationItem, ConversationRequest};
use axum::{Router, body::Body, http::HeaderMap, response::Response, routing::post};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::sync::Once;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug)]
struct StaticBearer(&'static str);

impl BearerResolver for StaticBearer {
    fn current_bearer(&self) -> Option<String> {
        Some(self.0.to_owned())
    }
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    headers: HeaderMap,
    body: Value,
}

fn completed_response(output: Value) -> Value {
    json!({
        "type": "response.completed",
        "sequence_number": 2,
        "response": {
            "id": "resp_compact_1",
            "object": "response",
            "created_at": 0,
            "model": "gpt-test",
            "status": "completed",
            "output": output,
            "usage": {
                "input_tokens": 120,
                "input_tokens_details": { "cached_tokens": 20 },
                "output_tokens": 8,
                "output_tokens_details": { "reasoning_tokens": 0 },
                "total_tokens": 128
            }
        }
    })
}

fn compact_sse() -> String {
    let item = json!({
        "type": "compaction",
        "id": "cmp_1",
        "encrypted_content": "opaque",
        "created_by": "server"
    });
    [
        format!(
            "event: response.output_item.done\ndata: {}\n\n",
            json!({
                "type": "response.output_item.done",
                "sequence_number": 1,
                "output_index": 0,
                "item": item
            })
        ),
        format!(
            "event: response.completed\ndata: {}\n\n",
            completed_response(json!([item]))
        ),
        "data: [DONE]\n\n".to_owned(),
    ]
    .concat()
}

async fn start_server(
    status: axum::http::StatusCode,
    response_body: String,
) -> (String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let handler_capture = captured.clone();
    let app = Router::new().route(
        "/v1/responses",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let handler_capture = handler_capture.clone();
            let response_body = response_body.clone();
            async move {
                let body: Value = serde_json::from_slice(&body).unwrap();
                handler_capture
                    .lock()
                    .unwrap()
                    .push(CapturedRequest { headers, body });
                Response::builder()
                    .status(status)
                    .header("content-type", "text/event-stream")
                    .body(Body::from(response_body))
                    .unwrap()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{address}/v1"), captured)
}

fn config(base_url: String) -> SamplerConfig {
    static REQUEST_AGENT: Once = Once::new();
    REQUEST_AGENT.call_once(|| {
        atelier_sampler::set_request_agent_identity("pi".into(), Some("1.0".into())).unwrap();
    });
    SamplerConfig {
        api_key: Some("stale-token".into()),
        base_url,
        model: "gpt-test".into(),
        api_backend: ApiBackend::Responses,
        auth_scheme: AuthScheme::Bearer,
        extra_headers: IndexMap::from([("x-provider-header".into(), "present".into())]),
        request_payload: serde_json::from_value(json!({
            "service_tier": "priority",
            "instructions": "payload must not replace typed compaction context",
            "input": []
        }))
        .unwrap(),
        remote_compaction_v2: true,
        bearer_resolver: Some(Arc::new(StaticBearer("fresh-token"))),
        ..Default::default()
    }
}

#[tokio::test]
async fn v2_compaction_streams_through_responses_with_trigger_tools_and_provider_controls() {
    let (base_url, captured) = start_server(axum::http::StatusCode::OK, compact_sse()).await;
    let client = RemoteCompactionV2Client::from_config(config(base_url))
        .unwrap()
        .expect("configured capability");

    let response = client
        .compact(
            ConversationRequest {
                items: vec![ConversationItem::user("hello")],
                tools: vec![atelier_sampling_types::ToolSpec {
                    name: "read".into(),
                    description: Some("read a file".into()),
                    parameters: json!({"type":"object"}),
                }],
                model: Some("gpt-test".into()),
                ..Default::default()
            },
            Some("preserve decisions".to_owned()),
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert_eq!(response.response_id, "resp_compact_1");
    assert_eq!(response.compaction.id.as_deref(), Some("cmp_1"));
    assert_eq!(response.compaction.encrypted_content, "opaque");
    assert_eq!(response.usage.unwrap().prompt_tokens, 120);

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.headers.get("authorization").unwrap(),
        "Bearer fresh-token"
    );
    assert_eq!(request.headers.get("x-provider-header").unwrap(), "present");
    assert!(
        request
            .headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("pi/1.0 (")
    );
    assert_eq!(request.body["model"], "gpt-test");
    assert_eq!(request.body["instructions"], "preserve decisions");
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["service_tier"], "priority");
    assert_eq!(
        request.body["input"].as_array().unwrap().last().unwrap(),
        &json!({"type": "compaction_trigger"})
    );
    assert_eq!(request.body["tools"].as_array().unwrap().len(), 1);
}

#[test]
fn v2_client_is_exact_opt_in_and_responses_only() {
    let mut disabled = config("https://provider.example/v1".into());
    disabled.remote_compaction_v2 = false;
    assert!(
        RemoteCompactionV2Client::from_config(disabled)
            .unwrap()
            .is_none()
    );

    let mut non_responses = config("https://provider.example/v1".into());
    non_responses.api_backend = ApiBackend::ChatCompletions;
    assert!(RemoteCompactionV2Client::from_config(non_responses).is_err());
}

fn compact_request() -> ConversationRequest {
    ConversationRequest {
        items: vec![ConversationItem::user("hello")],
        model: Some("gpt-test".into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn v2_requires_exactly_one_compaction_item_and_completed_event() {
    let terminal_only = format!(
        "event: response.completed\ndata: {}\n\ndata: [DONE]\n\n",
        completed_response(json!([]))
    );
    let (base_url, _) = start_server(axum::http::StatusCode::OK, terminal_only).await;
    let client = RemoteCompactionV2Client::from_config(config(base_url))
        .unwrap()
        .unwrap();
    let error = client
        .compact(compact_request(), None, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(error, SamplingError::Serialization(_)));

    let item = json!({
        "type": "compaction",
        "id": "cmp_duplicate",
        "encrypted_content": "opaque"
    });
    let duplicate = [
        format!(
            "event: response.output_item.done\ndata: {}\n\n",
            json!({
                "type": "response.output_item.done",
                "sequence_number": 1,
                "output_index": 0,
                "item": item
            })
        ),
        format!(
            "event: response.output_item.done\ndata: {}\n\n",
            json!({
                "type": "response.output_item.done",
                "sequence_number": 2,
                "output_index": 1,
                "item": item
            })
        ),
        format!(
            "event: response.completed\ndata: {}\n\n",
            completed_response(json!([item, item]))
        ),
        "data: [DONE]\n\n".to_owned(),
    ]
    .concat();
    let (base_url, _) = start_server(axum::http::StatusCode::OK, duplicate).await;
    let client = RemoteCompactionV2Client::from_config(config(base_url))
        .unwrap()
        .unwrap();
    let error = client
        .compact(compact_request(), None, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(error, SamplingError::Serialization(_)));

    let output_without_completion = format!(
        "event: response.output_item.done\ndata: {}\n\ndata: [DONE]\n\n",
        json!({
            "type": "response.output_item.done",
            "sequence_number": 1,
            "output_index": 0,
            "item": item
        })
    );
    let (base_url, _) = start_server(axum::http::StatusCode::OK, output_without_completion).await;
    let client = RemoteCompactionV2Client::from_config(config(base_url))
        .unwrap()
        .unwrap();
    let error = client
        .compact(compact_request(), None, Duration::from_secs(15))
        .await
        .unwrap_err();
    assert!(matches!(error, SamplingError::EventStreamError(_)));
}
