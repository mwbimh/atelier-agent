//! Unary OpenAI Images-compatible generation over the Provider transport.

use std::time::Duration;

use atelier_sampling_types::error::parse_error_bytes;
use atelier_sampling_types::{ApiBackend, Result, SamplingError};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::attribution::SamplingConsumer;
use crate::client::{
    SamplingClient, extract_model_metadata, extract_retry_after, extract_should_retry,
};
use crate::config::SamplerConfig;

const MAX_IMAGE_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct ImageGenerationRequest {
    pub model: String,
    pub prompt: String,
    pub response_format: &'static str,
}

impl std::fmt::Debug for ImageGenerationRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageGenerationRequest")
            .field("model", &self.model)
            .field("prompt", &"REDACTED")
            .field("response_format", &self.response_format)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedImage {
    pub b64_json: String,
}

#[derive(Deserialize)]
struct ImageGenerationResponse {
    #[serde(default)]
    data: Vec<ImageGenerationResponseItem>,
}

#[derive(Deserialize)]
struct ImageGenerationResponseItem {
    #[serde(default)]
    b64_json: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    url: Option<String>,
}

#[derive(Clone, Debug)]
struct ProviderRelativeImageEndpoint(String);

impl ProviderRelativeImageEndpoint {
    fn parse(raw: String) -> Result<Self> {
        let endpoint = raw.trim();
        let unsafe_segment = endpoint
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."));
        if endpoint.is_empty()
            || endpoint != raw
            || endpoint.starts_with(['/', '\\'])
            || endpoint.contains(['\\', '?', '#', '%'])
            || endpoint.chars().any(char::is_control)
            || reqwest::Url::parse(endpoint).is_ok()
            || unsafe_segment
        {
            return Err(SamplingError::InvalidConfiguration(
                "image generation endpoint must be a safe Provider-relative path",
            ));
        }
        Ok(Self(raw))
    }

    fn resolve(&self, base_url: &str) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(base_url).map_err(|_| {
            SamplingError::InvalidConfiguration("Provider base_url must be an absolute URL")
        })?;
        let scheme = url.scheme().to_owned();
        let host = url.host_str().map(str::to_owned);
        let port = url.port_or_known_default();
        let path = format!("{}/{}", url.path().trim_end_matches('/'), self.0.as_str());
        url.set_path(&path);
        url.set_query(None);
        url.set_fragment(None);
        if url.scheme() != scheme
            || url.host_str() != host.as_deref()
            || url.port_or_known_default() != port
        {
            return Err(SamplingError::InvalidConfiguration(
                "image generation endpoint must preserve the Provider origin",
            ));
        }
        Ok(url)
    }
}

/// Dedicated non-streaming client for an exact Provider/model image endpoint.
/// It reuses sampler auth, dynamic bearer resolution, Provider headers and the
/// process-wide shared HTTP client. It deliberately ignores inference
/// `request_payload` and performs exactly one HTTP request per call.
#[derive(Clone, Debug)]
pub struct ImageGenerationClient {
    sampling: SamplingClient,
    endpoint: ProviderRelativeImageEndpoint,
    model: String,
}

impl ImageGenerationClient {
    pub fn from_config(config: SamplerConfig) -> Result<Option<Self>> {
        let Some(raw_endpoint) = config.image_generation_endpoint.clone() else {
            return Ok(None);
        };
        if config.api_backend == ApiBackend::Messages {
            return Err(SamplingError::InvalidConfiguration(
                "image generation requires an OpenAI-compatible wire API",
            ));
        }
        let endpoint = ProviderRelativeImageEndpoint::parse(raw_endpoint)?;
        let model = config.model.clone();
        let sampling = SamplingClient::new(config)?;
        endpoint.resolve(sampling.base_url())?;
        Ok(Some(Self {
            sampling,
            endpoint,
            model,
        }))
    }

    pub async fn generate_image(
        &self,
        prompt: impl Into<String>,
        request_timeout: Duration,
    ) -> Result<GeneratedImage> {
        self.generate_image_request(
            ImageGenerationRequest {
                model: self.model.clone(),
                prompt: prompt.into(),
                response_format: "b64_json",
            },
            request_timeout,
        )
        .await
    }

    pub async fn generate_image_request(
        &self,
        request: ImageGenerationRequest,
        request_timeout: Duration,
    ) -> Result<GeneratedImage> {
        if request.model != self.model {
            return Err(SamplingError::InvalidConfiguration(
                "image generation request model must match the Provider model",
            ));
        }
        if request.response_format != "b64_json" {
            return Err(SamplingError::InvalidConfiguration(
                "image generation response_format must be b64_json",
            ));
        }
        let url = self.endpoint.resolve(self.sampling.base_url())?;
        tracing::info!(
            target: crate::sampling_log::TARGET,
            event = "image_generation_request",
            base_url = %self.sampling.base_url(),
            model = %self.model,
        );
        let response = self
            .sampling
            .post_provider_url(url)
            .timeout(request_timeout)
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = read_response_limited(response, MAX_IMAGE_RESPONSE_BYTES).await?;

        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.sampling
                .record_401_attribution(SamplingConsumer::ImageGeneration);
            return Err(SamplingError::Auth(format!(
                "Unauthorized (401): {}",
                parse_error_bytes(bytes.as_ref())
            )));
        }
        if !status.is_success() {
            return Err(SamplingError::Api {
                status,
                message: parse_error_bytes(bytes.as_ref()),
                model_metadata,
                retry_after_secs,
                should_retry,
            });
        }

        let response: ImageGenerationResponse = serde_json::from_slice(&bytes)?;
        let b64_json = response
            .data
            .into_iter()
            .find_map(|item| item.b64_json)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                SamplingError::serialization_message(
                    "image generation response must contain data[].b64_json",
                )
            })?;
        Ok(GeneratedImage { b64_json })
    }
}

async fn read_response_limited(response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(SamplingError::serialization_message(
            "image generation response exceeds the size limit",
        ));
    }
    let mut stream = response.bytes_stream();
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if output.len().saturating_add(chunk.len()) > max_bytes {
            return Err(SamplingError::serialization_message(
                "image generation response exceeds the size limit",
            ));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::ProviderRelativeImageEndpoint;

    #[test]
    fn provider_relative_endpoint_preserves_base_path_and_origin() {
        let endpoint =
            ProviderRelativeImageEndpoint::parse("images/generations".to_owned()).unwrap();
        let url = endpoint.resolve("https://api.example.test/v1").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.example.test/v1/images/generations"
        );
    }
}
