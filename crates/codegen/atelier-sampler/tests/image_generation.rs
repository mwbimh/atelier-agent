use atelier_sampler::{
    ApiBackend, AuthScheme, BearerResolver, ImageGenerationClient, SamplerConfig, SamplingError,
};
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
        "/v1/images/generations",
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
        model: "gpt-image-test".into(),
        api_backend: ApiBackend::Responses,
        auth_scheme: AuthScheme::Bearer,
        extra_headers: IndexMap::from([("x-provider-header".into(), "present".into())]),
        request_payload: serde_json::from_value(json!({
            "fast_mode": true,
            "provider_option": {"must_not_leak": true}
        }))
        .unwrap(),
        image_generation_endpoint: Some("images/generations".into()),
        bearer_resolver: Some(Arc::new(StaticBearer("fresh-token"))),
        max_retries: Some(9),
        ..Default::default()
    }
}

#[tokio::test]
async fn generate_image_reuses_provider_transport_and_ignores_inference_payload() {
    let (base_url, captured) = start_server(
        axum::http::StatusCode::OK,
        json!({"data": [{"b64_json": "aW1hZ2U="}]}),
    )
    .await;
    let client = ImageGenerationClient::from_config(config(base_url))
        .unwrap()
        .expect("configured image endpoint");

    let generated = client
        .generate_image("a blue fox", Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(generated.b64_json, "aW1hZ2U=");
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let request = &captured[0];
    assert_eq!(request.body["model"], "gpt-image-test");
    assert_eq!(request.body["prompt"], "a blue fox");
    assert_eq!(request.body["response_format"], "b64_json");
    assert_eq!(request.body.as_object().unwrap().len(), 3);
    assert_eq!(
        request.headers["authorization"], "Bearer fresh-token",
        "dynamic bearer must replace the construction-time token"
    );
    assert_eq!(request.headers["x-provider-header"], "present");
    assert!(
        request.headers["user-agent"]
            .to_str()
            .unwrap()
            .starts_with("pi/1.0 (")
    );
}

#[tokio::test]
async fn absent_endpoint_constructs_no_client_and_makes_zero_requests() {
    let (base_url, captured) = start_server(
        axum::http::StatusCode::OK,
        json!({"data": [{"b64_json": "unused"}]}),
    )
    .await;
    let mut config = config(base_url);
    config.image_generation_endpoint = None;

    assert!(
        ImageGenerationClient::from_config(config)
            .unwrap()
            .is_none()
    );
    tokio::task::yield_now().await;
    assert!(captured.lock().unwrap().is_empty());
}

#[test]
fn unsafe_or_non_openai_image_endpoints_are_rejected() {
    for endpoint in [
        "https://evil.example/images",
        "/images/generations",
        "../images/generations",
        "images/../generations",
        r"images\generations",
        "images/%2e%2e/generations",
        "images/generations?target=evil",
        "images/generations#fragment",
    ] {
        let mut unsafe_config = config("https://provider.example/v1".into());
        unsafe_config.image_generation_endpoint = Some(endpoint.into());
        assert!(
            ImageGenerationClient::from_config(unsafe_config).is_err(),
            "unsafe endpoint should fail: {endpoint:?}"
        );
    }

    let mut messages = config("https://provider.example/v1".into());
    messages.api_backend = ApiBackend::Messages;
    assert!(ImageGenerationClient::from_config(messages).is_err());
}

#[tokio::test]
async fn url_only_image_response_is_rejected() {
    let (base_url, _) = start_server(
        axum::http::StatusCode::OK,
        json!({"data": [{"url": "https://images.example/result.png"}]}),
    )
    .await;
    let client = ImageGenerationClient::from_config(config(base_url))
        .unwrap()
        .unwrap();

    let error = client
        .generate_image("no download", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(error, SamplingError::Serialization(_)));
}

#[tokio::test]
async fn image_generation_is_not_automatically_retried() {
    let (base_url, captured) = start_server(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        json!({"error": {"message": "failed once"}}),
    )
    .await;
    let client = ImageGenerationClient::from_config(config(base_url))
        .unwrap()
        .unwrap();

    let error = client
        .generate_image("one request only", Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(error, SamplingError::Api { .. }));
    assert_eq!(captured.lock().unwrap().len(), 1);
}
