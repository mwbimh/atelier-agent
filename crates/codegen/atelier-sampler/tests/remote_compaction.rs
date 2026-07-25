use atelier_sampler::{
    ApiBackend, AuthScheme, BearerResolver, CompactClient, SamplerConfig, SamplingError,
};
use atelier_sampling_types::rs;
use axum::{Json, Router, http::HeaderMap, routing::post};
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

async fn start_server(
    status: axum::http::StatusCode,
    response: Value,
) -> (String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let handler_capture = captured.clone();
    let app = Router::new().route(
        "/v1/responses/compact",
        post(move |headers: HeaderMap, Json(body): Json<Value>| {
            let handler_capture = handler_capture.clone();
            let response = response.clone();
            async move {
                handler_capture
                    .lock()
                    .unwrap()
                    .push(CapturedRequest { headers, body });
                (status, Json(response))
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
            "fast_mode": true,
            "temperature": 0.1
        }))
        .unwrap(),
        remote_compaction_endpoint: Some("responses/compact".into()),
        bearer_resolver: Some(Arc::new(StaticBearer("fresh-token"))),
        ..Default::default()
    }
}

#[tokio::test]
async fn unary_compaction_uses_provider_auth_and_never_merges_inference_payload() {
    let (base_url, captured) = start_server(
        axum::http::StatusCode::OK,
        json!({
            "output": [{
                "type": "compaction",
                "id": "cmp_1",
                "encrypted_content": "opaque"
            }]
        }),
    )
    .await;
    let client = CompactClient::from_config(config(base_url))
        .unwrap()
        .expect("configured endpoint");

    let response = client
        .compact(
            vec![
                serde_json::from_value::<rs::InputItem>(json!({
                    "type": "message",
                    "role": "user",
                    "content": "hello"
                }))
                .unwrap(),
            ],
            "compact carefully",
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert!(matches!(
        &response.output[0],
        rs::InputItem::Item(rs::Item::Compaction(compaction))
            if compaction.id.as_deref() == Some("cmp_1")
                && compaction.encrypted_content == "opaque"
    ));
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
    assert_eq!(
        request.body,
        json!({
            "model": "gpt-test",
            "input": [{"type": "message", "role": "user", "content": "hello"}],
            "instructions": "compact carefully"
        })
    );
    assert!(request.body.get("fast_mode").is_none());
    assert!(request.body.get("temperature").is_none());
    assert!(request.body.get("stream").is_none());
    assert!(request.body.get("tools").is_none());
}

#[test]
fn compact_client_rejects_non_responses_and_unsafe_endpoints() {
    let mut non_responses = config("https://provider.example/v1".into());
    non_responses.api_backend = ApiBackend::ChatCompletions;
    assert!(CompactClient::from_config(non_responses).is_err());

    for endpoint in [
        "https://evil.example/compact",
        "/responses/compact",
        "../responses/compact",
        "responses/../compact",
        r"responses\compact",
        "responses/%2e%2e/compact",
        "responses/compact?target=evil",
        "responses/compact#fragment",
    ] {
        let mut unsafe_config = config("https://provider.example/v1".into());
        unsafe_config.remote_compaction_endpoint = Some(endpoint.into());
        assert!(
            CompactClient::from_config(unsafe_config).is_err(),
            "unsafe endpoint should fail: {endpoint:?}"
        );
    }
}

#[test]
fn absent_endpoint_does_not_construct_a_compact_client() {
    let mut config = config("https://provider.example/v1".into());
    config.remote_compaction_endpoint = None;
    assert!(CompactClient::from_config(config).unwrap().is_none());
}

#[tokio::test]
async fn compact_client_classifies_rate_limits_and_rejects_empty_output() {
    let (base_url, _) = start_server(
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        json!({"error": {"message": "slow down"}}),
    )
    .await;
    let client = CompactClient::from_config(config(base_url))
        .unwrap()
        .unwrap();
    let error = client
        .compact(Vec::new(), "", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        SamplingError::Api {
            status: axum::http::StatusCode::TOO_MANY_REQUESTS,
            ..
        }
    ));

    let (base_url, _) = start_server(axum::http::StatusCode::OK, json!({"output": []})).await;
    let client = CompactClient::from_config(config(base_url))
        .unwrap()
        .unwrap();
    let error = client
        .compact(Vec::new(), "", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(error, SamplingError::Serialization(_)));
}
