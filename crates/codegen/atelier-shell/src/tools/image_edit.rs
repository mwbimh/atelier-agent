//! Adapter from the provider-backed sampler image-edit client to `atelier-tools`.

use std::sync::Arc;
use std::time::Duration;

use atelier_sampler::{ImageEditClient, SamplerConfig, SamplingError};
use atelier_tools::implementations::atelier_build::image_edit::{
    ImageEditConfig, ImageEditExecutor, ImageEditExecutorError, ImageEditExecutorResponse,
    ImageEditRequest,
};

const IMAGE_EDIT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
struct SamplerImageEditExecutor {
    client: ImageEditClient,
}

#[async_trait::async_trait]
impl ImageEditExecutor for SamplerImageEditExecutor {
    async fn execute(
        &self,
        request: ImageEditRequest,
    ) -> Result<ImageEditExecutorResponse, ImageEditExecutorError> {
        let edited = self
            .client
            .edit_image(
                atelier_sampler::ImageEditRequest {
                    prompt: request.prompt,
                    images: request
                        .images
                        .into_iter()
                        .map(|image| atelier_sampler::ImageEditReference {
                            bytes: image.bytes,
                            mime_type: image.mime_type,
                        })
                        .collect(),
                },
                IMAGE_EDIT_TIMEOUT,
            )
            .await
            .map_err(|_| ImageEditExecutorError::new("image edit request failed"))?;
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [{"b64_json": edited.b64_json}],
        }))
        .map_err(|_| ImageEditExecutorError::new("image edit response encoding failed"))?;
        Ok(ImageEditExecutorResponse::new(body))
    }
}

pub(crate) fn sampler_config_for_route(
    mut config: SamplerConfig,
    route: Option<atelier_provider::ResolvedImageEditRoute>,
) -> (SamplerConfig, Option<String>) {
    let mut endpoint = None;
    if let Some(route) = route {
        match route.adapter {
            atelier_provider::MediaAdapter::OpenAiImages => {
                config.model = route.model.model_id;
                config.api_backend = atelier_sampling_types::ApiBackend::ChatCompletions;
                endpoint = Some(route.endpoint);
            }
        }
    }
    (config, endpoint)
}

pub(crate) fn config_from_sampler(
    config: SamplerConfig,
    endpoint: Option<String>,
) -> Result<ImageEditConfig, SamplingError> {
    let Some(client) = ImageEditClient::from_config(config, endpoint)? else {
        return Ok(ImageEditConfig::Disabled);
    };
    Ok(ImageEditConfig::enabled(Arc::new(
        SamplerImageEditExecutor { client },
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_exact_endpoint_disables_tool() {
        let config = config_from_sampler(SamplerConfig::default(), None).unwrap();
        assert!(!config.is_enabled());
    }

    #[test]
    fn independent_provider_route_replaces_the_inference_model() {
        let (config, endpoint) = sampler_config_for_route(
            SamplerConfig {
                provider_id: Some("example".to_owned()),
                model: "reasoning-model".to_owned(),
                ..SamplerConfig::default()
            },
            Some(atelier_provider::ResolvedImageEditRoute {
                model: atelier_provider::ModelKey::new("example", "image-model").unwrap(),
                adapter: atelier_provider::MediaAdapter::OpenAiImages,
                endpoint: "images/edits".to_owned(),
            }),
        );
        assert_eq!(config.model, "image-model");
        assert_eq!(endpoint.as_deref(), Some("images/edits"));
    }

    #[test]
    fn exact_endpoint_constructs_sampler_backed_executor_without_request() {
        let config = config_from_sampler(
            SamplerConfig {
                base_url: "https://provider.example/v1".to_owned(),
                model: "image-model".to_owned(),
                ..SamplerConfig::default()
            },
            Some("images/edits".to_owned()),
        )
        .unwrap();
        assert!(config.is_enabled());
    }
}
