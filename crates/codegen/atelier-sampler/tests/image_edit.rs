use atelier_sampler::{
    ApiBackend, AuthScheme, BearerResolver, ImageEditClient, ImageEditReference, ImageEditRequest,
    SamplerConfig, SamplingError,
};
use axum::{Router, body::Bytes, http::HeaderMap, routing::post};
use indexmap::IndexMap;
use serde_json::json;
use std::sync::{Arc, Mutex, Once};
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
    body: Vec<u8>,
}

async fn start_server(
    status: axum::http::StatusCode,
) -> (String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let handler_capture = captured.clone();
    let app = Router::new().route(
        "/v1/images/edits",
        post(move |headers: HeaderMap, body: Bytes| {
            let handler_capture = handler_capture.clone();
            async move {
                handler_capture.lock().unwrap().push(CapturedRequest {
                    headers,
                    body: body.to_vec(),
                });
                (
                    status,
                    axum::Json(json!({"data": [{"b64_json": "ZWRpdGVk"}]})),
                )
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
            "provider_option": {"must_not_leak": true}
        }))
        .unwrap(),
        bearer_resolver: Some(Arc::new(StaticBearer("fresh-token"))),
        ..Default::default()
    }
}

fn png_reference(bytes: &[u8]) -> ImageEditReference {
    ImageEditReference {
        bytes: bytes.to_vec(),
        mime_type: "image/png".into(),
    }
}

#[tokio::test]
async fn edit_image_uses_provider_transport_and_openai_multipart_shape() {
    let (base_url, captured) = start_server(axum::http::StatusCode::OK).await;
    let client = ImageEditClient::from_config(config(base_url), Some("images/edits".into()))
        .unwrap()
        .expect("configured edit endpoint");

    let edited = client
        .edit_image(
            ImageEditRequest {
                prompt: "make the fox blue".into(),
                images: vec![png_reference(b"png-one"), png_reference(b"png-two")],
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap();

    assert_eq!(edited.b64_json, "ZWRpdGVk");
    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let request = &captured[0];
    assert_eq!(request.headers["authorization"], "Bearer fresh-token");
    assert_eq!(request.headers["x-provider-header"], "present");
    let content_type = request.headers["content-type"].to_str().unwrap();
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "unexpected content type: {content_type}"
    );
    let body = String::from_utf8_lossy(&request.body);
    assert!(body.contains("name=\"model\""));
    assert!(body.contains("gpt-image-test"));
    assert!(body.contains("name=\"prompt\""));
    assert!(body.contains("make the fox blue"));
    assert!(body.contains("name=\"response_format\""));
    assert!(body.contains("b64_json"));
    assert_eq!(body.matches("name=\"image[]\"").count(), 2);
    assert!(body.contains("png-one"));
    assert!(body.contains("png-two"));
    assert!(!body.contains("must_not_leak"));
}

#[tokio::test]
async fn image_edit_is_not_automatically_retried() {
    let (base_url, captured) = start_server(axum::http::StatusCode::SERVICE_UNAVAILABLE).await;
    let client = ImageEditClient::from_config(config(base_url), Some("images/edits".into()))
        .unwrap()
        .unwrap();

    let error = client
        .edit_image(
            ImageEditRequest {
                prompt: "one request only".into(),
                images: vec![png_reference(b"png")],
            },
            Duration::from_secs(5),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SamplingError::Api { .. }));
    assert_eq!(captured.lock().unwrap().len(), 1);
}

#[test]
fn unsafe_endpoint_or_unsupported_reference_fails_closed() {
    assert!(
        ImageEditClient::from_config(
            config("https://provider.example/v1".into()),
            Some("../images/edits".into())
        )
        .is_err()
    );

    let client = ImageEditClient::from_config(
        config("https://provider.example/v1".into()),
        Some("images/edits".into()),
    )
    .unwrap()
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(client.edit_image(
            ImageEditRequest {
                prompt: "edit".into(),
                images: vec![ImageEditReference {
                    bytes: vec![1, 2, 3],
                    mime_type: "image/gif".into(),
                }],
            },
            Duration::from_secs(1),
        ))
        .unwrap_err();
    assert!(matches!(error, SamplingError::InvalidConfiguration(_)));
}
