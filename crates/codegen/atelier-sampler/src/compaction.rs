//! Unary transport for Provider-specific Responses-compatible compaction.

use std::time::Duration;

use atelier_sampling_types::error::parse_error_bytes;
use atelier_sampling_types::{ApiBackend, Result, SamplingError, rs};
use serde::{Deserialize, Serialize};

use crate::attribution::SamplingConsumer;
use crate::client::{
    SamplingClient, extract_model_metadata, extract_retry_after, extract_should_retry,
};
use crate::config::SamplerConfig;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactFailureAction {
    FallbackLocal,
    ReturnError,
}

pub fn classify_compact_failure(error: &SamplingError) -> CompactFailureAction {
    match error {
        SamplingError::Http(_) | SamplingError::Serialization(_) => {
            CompactFailureAction::FallbackLocal
        }
        SamplingError::Api { status, .. }
            if matches!(status.as_u16(), 404 | 405 | 501) || status.is_server_error() =>
        {
            CompactFailureAction::FallbackLocal
        }
        SamplingError::Auth(_)
        | SamplingError::InvalidConfiguration(_)
        | SamplingError::Api { .. }
        | SamplingError::EventStreamError(_)
        | SamplingError::StreamError { .. }
        | SamplingError::IdleTimeout { .. }
        | SamplingError::EmptyResponse { .. }
        | SamplingError::MaxTokensTruncation
        | SamplingError::DoomLoopDetected { .. } => CompactFailureAction::ReturnError,
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct CompactRequest {
    pub model: String,
    pub input: Vec<rs::InputItem>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub instructions: String,
}

impl std::fmt::Debug for CompactRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompactRequest")
            .field("model", &self.model)
            .field("input_items", &self.input.len())
            .field("instructions", &"REDACTED")
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct CompactResponse {
    pub output: Vec<rs::InputItem>,
}

impl std::fmt::Debug for CompactResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompactResponse")
            .field("output_items", &self.output.len())
            .finish()
    }
}

#[derive(Clone, Debug)]
struct ProviderRelativeEndpoint(String);

impl ProviderRelativeEndpoint {
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
                "remote compaction endpoint must be a safe Provider-relative path",
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
                "remote compaction endpoint must preserve the Provider origin",
            ));
        }
        Ok(url)
    }
}

/// Dedicated non-streaming client for the experimental remote compaction
/// endpoint. It reuses the Provider's HTTP client, authentication and headers,
/// but deliberately ignores ordinary inference `request_payload` fields.
#[derive(Clone, Debug)]
pub struct CompactClient {
    sampling: SamplingClient,
    endpoint: ProviderRelativeEndpoint,
    model: String,
}

impl CompactClient {
    /// Construct a client only when a remote endpoint is configured. A
    /// non-Responses backend is rejected even if an endpoint somehow bypasses
    /// the Provider control-plane gate.
    pub fn from_config(config: SamplerConfig) -> Result<Option<Self>> {
        let Some(raw_endpoint) = config.remote_compaction_endpoint.clone() else {
            return Ok(None);
        };
        if config.api_backend != ApiBackend::Responses {
            return Err(SamplingError::InvalidConfiguration(
                "remote compaction requires the Responses wire API",
            ));
        }
        let endpoint = ProviderRelativeEndpoint::parse(raw_endpoint)?;
        let model = config.model.clone();
        let sampling = SamplingClient::new(config)?;
        endpoint.resolve(sampling.base_url())?;
        Ok(Some(Self {
            sampling,
            endpoint,
            model,
        }))
    }

    pub async fn compact(
        &self,
        input: Vec<rs::InputItem>,
        instructions: impl Into<String>,
        request_timeout: Duration,
    ) -> Result<CompactResponse> {
        self.compact_request(
            CompactRequest {
                model: self.model.clone(),
                input,
                instructions: instructions.into(),
            },
            request_timeout,
        )
        .await
    }

    pub async fn compact_request(
        &self,
        request: CompactRequest,
        request_timeout: Duration,
    ) -> Result<CompactResponse> {
        if request.model != self.model {
            return Err(SamplingError::InvalidConfiguration(
                "remote compaction request model must match the Provider model",
            ));
        }
        let url = self.endpoint.resolve(self.sampling.base_url())?;
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
        let bytes = response.bytes().await?;

        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.sampling
                .record_401_attribution(SamplingConsumer::RemoteCompaction);
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

        let response: CompactResponse = serde_json::from_slice(&bytes)?;
        if response.output.is_empty() {
            return Err(SamplingError::serialization_message(
                "remote compaction response output must not be empty",
            ));
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::{CompactFailureAction, ProviderRelativeEndpoint, classify_compact_failure};
    use atelier_sampling_types::SamplingError;
    use reqwest::StatusCode;

    fn api_error(status: StatusCode) -> SamplingError {
        SamplingError::Api {
            status,
            message: "test".to_owned(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
        }
    }

    #[test]
    fn provider_relative_endpoint_preserves_base_path_and_origin() {
        let endpoint = ProviderRelativeEndpoint::parse("responses/compact".to_owned()).unwrap();
        let url = endpoint.resolve("https://api.example.test/v1").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.example.test/v1/responses/compact"
        );
    }

    #[test]
    fn remote_compaction_failure_contract_is_exact() {
        for status in [
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::NOT_IMPLEMENTED,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
        ] {
            assert_eq!(
                classify_compact_failure(&api_error(status)),
                CompactFailureAction::FallbackLocal
            );
        }
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert_eq!(
                classify_compact_failure(&api_error(status)),
                CompactFailureAction::ReturnError
            );
        }
        assert_eq!(
            classify_compact_failure(&SamplingError::serialization_message("decode")),
            CompactFailureAction::FallbackLocal
        );
        assert_eq!(
            classify_compact_failure(&SamplingError::InvalidConfiguration("bad endpoint")),
            CompactFailureAction::ReturnError
        );
    }
}
