//! Adapter from the provider-backed sampler image client to `atelier-tools`.

use std::sync::Arc;
use std::time::Duration;

use atelier_sampler::{ImageGenerationClient, SamplerConfig, SamplingError};
use atelier_tools::implementations::atelier_build::image_gen::{
    ImageGenConfig, ImageGenExecutor, ImageGenExecutorError, ImageGenExecutorResponse,
    ImageGenRequest,
};

const IMAGE_GENERATION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
struct SamplerImageGenExecutor {
    client: ImageGenerationClient,
}

#[async_trait::async_trait]
impl ImageGenExecutor for SamplerImageGenExecutor {
    async fn execute(
        &self,
        request: ImageGenRequest,
    ) -> Result<ImageGenExecutorResponse, ImageGenExecutorError> {
        let generated = self
            .client
            .generate_image(request.prompt, IMAGE_GENERATION_TIMEOUT)
            .await
            .map_err(|_| ImageGenExecutorError::new("image generation request failed"))?;
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [{"b64_json": generated.b64_json}],
        }))
        .map_err(|_| ImageGenExecutorError::new("image generation response encoding failed"))?;
        Ok(ImageGenExecutorResponse::new(body))
    }
}

/// Build the tool configuration for one exact Provider/model sampler route.
/// Absence of an endpoint disables the tool; invalid routes fail closed.
pub(crate) fn config_from_sampler(config: SamplerConfig) -> Result<ImageGenConfig, SamplingError> {
    let Some(client) = ImageGenerationClient::from_config(config)? else {
        return Ok(ImageGenConfig::Disabled);
    };
    Ok(ImageGenConfig::enabled(Arc::new(SamplerImageGenExecutor {
        client,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_exact_endpoint_disables_tool() {
        let config = config_from_sampler(SamplerConfig::default()).unwrap();
        assert!(!config.is_enabled());
    }

    #[test]
    fn exact_endpoint_constructs_sampler_backed_executor_without_request() {
        let config = config_from_sampler(SamplerConfig {
            base_url: "https://provider.example/v1".to_owned(),
            model: "image-model".to_owned(),
            image_generation_endpoint: Some("images/generations".to_owned()),
            ..SamplerConfig::default()
        })
        .unwrap();
        assert!(config.is_enabled());
    }
}
