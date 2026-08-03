//! Provider-agnostic image editing from current-turn attachment references.

use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use super::image_gen::{ImageGenLimits, decode_response, save_image_atomically};
use crate::types::output::{MediaGenOutput, ToolOutput};
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::{AttachedImages, SessionFolder};
use crate::types::tool::{ToolKind, ToolNamespace};

pub const IMAGE_EDIT_TOOL_NAME: &str = "image_edit";
const MAX_EDIT_REFERENCES: usize = 16;
const MAX_REFERENCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_REFERENCE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEditReference {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageEditRequest {
    pub prompt: String,
    pub images: Vec<ImageEditReference>,
}

#[derive(Debug, Clone)]
pub struct ImageEditExecutorResponse {
    body: Vec<u8>,
}

impl ImageEditExecutorResponse {
    pub fn new(body: impl Into<Vec<u8>>) -> Self {
        Self { body: body.into() }
    }

    fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ImageEditExecutorError {
    message: String,
}

impl ImageEditExecutorError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait::async_trait]
pub trait ImageEditExecutor: Send + Sync + 'static {
    async fn execute(
        &self,
        request: ImageEditRequest,
    ) -> Result<ImageEditExecutorResponse, ImageEditExecutorError>;
}

#[derive(Clone)]
pub enum ImageEditConfig {
    Disabled,
    Executor {
        executor: Arc<dyn ImageEditExecutor>,
        limits: ImageGenLimits,
    },
}

impl Default for ImageEditConfig {
    fn default() -> Self {
        Self::Disabled
    }
}

impl std::fmt::Debug for ImageEditConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("ImageEditConfig::Disabled"),
            Self::Executor { limits, .. } => formatter
                .debug_struct("ImageEditConfig::Executor")
                .field("limits", limits)
                .finish_non_exhaustive(),
        }
    }
}

impl ImageEditConfig {
    pub fn enabled(executor: Arc<dyn ImageEditExecutor>) -> Self {
        Self::Executor {
            executor,
            limits: ImageGenLimits::default(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Executor { .. })
    }

    pub(crate) fn runtime(&self) -> Option<ImageEditRuntime> {
        match self {
            Self::Disabled => None,
            Self::Executor { executor, limits } => {
                Some(ImageEditRuntime::new(executor.clone(), *limits))
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ImageEditRuntime {
    executor: Arc<dyn ImageEditExecutor>,
    limits: ImageGenLimits,
}

impl ImageEditRuntime {
    pub(crate) fn new(executor: Arc<dyn ImageEditExecutor>, limits: ImageGenLimits) -> Self {
        Self { executor, limits }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageEditInput {
    /// Description of the desired edit or transformation.
    pub prompt: String,
    /// Current-turn attachment tokens such as `[Image #1]`. Multiple
    /// references are accepted in priority order.
    pub image: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageEditToolOutput(pub MediaGenOutput);

impl atelier_tool_runtime::ToolOutput for ImageEditToolOutput {}

impl From<ImageEditToolOutput> for ToolOutput {
    fn from(output: ImageEditToolOutput) -> Self {
        Self::ImageEdit(output.0)
    }
}

#[derive(Debug, Default)]
pub struct ImageEditTool;

impl crate::types::tool_metadata::ToolMetadata for ImageEditTool {
    fn kind(&self) -> ToolKind {
        ToolKind::ImageGen
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::AtelierBuild
    }

    fn description_template(&self) -> &str {
        "Edit one or more images attached to the current user message. Pass attachment tokens such as `[Image #1]`; never invent filesystem paths."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl atelier_tool_runtime::Tool for ImageEditTool {
    type Args = ImageEditInput;
    type Output = ImageEditToolOutput;

    fn id(&self) -> atelier_tool_protocol::ToolId {
        atelier_tool_protocol::ToolId::new(IMAGE_EDIT_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &atelier_tool_runtime::ListToolsContext,
    ) -> atelier_tool_types::ToolDescription {
        atelier_tool_types::ToolDescription::new(
            IMAGE_EDIT_TOOL_NAME,
            crate::types::tool_metadata::ToolMetadata::description_template(self),
        )
    }

    fn capabilities(&self) -> atelier_tool_protocol::ToolCapabilities {
        atelier_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(atelier_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.image_edit", skip_all)]
    async fn run(
        &self,
        ctx: atelier_tool_runtime::ToolCallContext,
        input: ImageEditInput,
    ) -> Result<ImageEditToolOutput, atelier_tool_runtime::ToolError> {
        if input.prompt.trim().is_empty() {
            return Err(tool_error("invalid_prompt", "prompt must not be empty"));
        }
        if input.image.is_empty() || input.image.len() > MAX_EDIT_REFERENCES {
            return Err(tool_error(
                "invalid_image_references",
                "image_edit requires between 1 and 16 current-turn attachment references",
            ));
        }

        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let (runtime, session_folder, attached) = {
            let resources = resources.lock().await;
            (
                resources.require::<ImageEditRuntime>()?.clone(),
                resources.require::<SessionFolder>()?.0.clone(),
                resources.require::<AttachedImages>()?.clone(),
            )
        };
        let images = load_attachment_references(&input.image, &attached).await?;
        let response = runtime
            .executor
            .execute(ImageEditRequest {
                prompt: input.prompt,
                images,
            })
            .await
            .map_err(|_| tool_error("image_edit_failed", "image edit executor failed"))?;
        let image = decode_response(response.body(), runtime.limits)?;
        let path = save_image_atomically(&session_folder, &image.bytes, image.extension).await?;
        Ok(ImageEditToolOutput(MediaGenOutput::new(path)))
    }
}

async fn load_attachment_references(
    tokens: &[String],
    attached: &AttachedImages,
) -> Result<Vec<ImageEditReference>, atelier_tool_runtime::ToolError> {
    let mut output = Vec::with_capacity(tokens.len());
    let mut total_bytes = 0usize;
    for token in tokens {
        let display_number = parse_attachment_token(token).ok_or_else(|| {
            tool_error(
                "invalid_image_reference",
                format!("{token:?} is not a current-turn attachment token such as [Image #1]"),
            )
        })?;
        let reference = attached.reference_for(display_number).ok_or_else(|| {
            let available = attached
                .0
                .iter()
                .map(|(number, _)| format!("[Image #{number}]"))
                .collect::<Vec<_>>()
                .join(", ");
            tool_error(
                "unknown_image_reference",
                format!(
                    "{token:?} does not match an image attached to the current message; available: {available}"
                ),
            )
        })?;
        let bytes = load_reference_bytes(reference).await?;
        if bytes.is_empty() || bytes.len() > MAX_REFERENCE_BYTES {
            return Err(tool_error(
                "image_reference_too_large",
                "each image edit reference must contain at most 16 MiB",
            ));
        }
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > MAX_TOTAL_REFERENCE_BYTES {
            return Err(tool_error(
                "image_references_too_large",
                "image edit references exceed the 32 MiB total limit",
            ));
        }
        let (_, _, mime_type) = crate::util::image_validate::validate_image_bytes_with(
            &bytes, false,
        )
        .map_err(|_| {
            tool_error(
                "invalid_image_reference",
                "attachment reference is not a valid image",
            )
        })?;
        if !matches!(mime_type, "image/png" | "image/jpeg" | "image/webp") {
            return Err(tool_error(
                "unsupported_image_reference",
                "image_edit references must be PNG, JPEG, or WebP",
            ));
        }
        output.push(ImageEditReference {
            bytes,
            mime_type: mime_type.to_owned(),
        });
    }
    Ok(output)
}

async fn load_reference_bytes(reference: &str) -> Result<Vec<u8>, atelier_tool_runtime::ToolError> {
    if let Some(rest) = reference.strip_prefix("data:") {
        let (metadata, encoded) = rest.split_once(',').ok_or_else(|| {
            tool_error("invalid_image_reference", "malformed attachment data URL")
        })?;
        if !metadata.ends_with(";base64") {
            return Err(tool_error(
                "invalid_image_reference",
                "attachment data URL must use base64 encoding",
            ));
        }
        return STANDARD.decode(encoded).map_err(|_| {
            tool_error(
                "invalid_image_reference",
                "attachment data URL contains invalid base64",
            )
        });
    }
    let path = Path::new(reference);
    if !path.is_absolute() {
        return Err(tool_error(
            "invalid_image_reference",
            "attached image path must be absolute",
        ));
    }
    tokio::fs::read(path).await.map_err(|_| {
        tool_error(
            "invalid_image_reference",
            "attached image file is no longer readable",
        )
    })
}

fn parse_attachment_token(value: &str) -> Option<usize> {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed)
        .trim();
    let rest = if inner
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("image"))
    {
        inner.get(5..)?.trim_start()
    } else {
        inner
    };
    rest.strip_prefix('#')?
        .trim()
        .parse()
        .ok()
        .filter(|n| *n > 0)
}

pub(crate) fn is_image_edit_tool_id(id: &str) -> bool {
    id == "AtelierBuild:image_edit"
}

fn tool_error(code: &'static str, message: impl Into<String>) -> atelier_tool_runtime::ToolError {
    atelier_tool_runtime::ToolError::custom(code, message.into())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::types::resources::Resources;
    use crate::types::tool_metadata::test_ctx_with_call_id;

    #[derive(Debug)]
    struct FakeExecutor {
        requests: Mutex<Vec<ImageEditRequest>>,
        response: Mutex<Option<ImageEditExecutorResponse>>,
    }

    #[async_trait::async_trait]
    impl ImageEditExecutor for FakeExecutor {
        async fn execute(
            &self,
            request: ImageEditRequest,
        ) -> Result<ImageEditExecutorResponse, ImageEditExecutorError> {
            self.requests.lock().unwrap().push(request);
            Ok(self.response.lock().unwrap().take().unwrap())
        }
    }

    fn png_bytes() -> Vec<u8> {
        let image = image::DynamicImage::new_rgba8(2, 2);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn attachment_token_parser_is_strict_but_case_insensitive() {
        assert_eq!(parse_attachment_token("[Image #1]"), Some(1));
        assert_eq!(parse_attachment_token("image #2"), Some(2));
        assert_eq!(parse_attachment_token("#3"), Some(3));
        assert_eq!(parse_attachment_token("/tmp/image.png"), None);
        assert_eq!(parse_attachment_token("Image #0"), None);
    }

    #[tokio::test]
    async fn resolves_multiple_current_turn_attachments_and_saves_typed_output() {
        let tmp = tempfile::tempdir().unwrap();
        let source = png_bytes();
        let source_path = tmp.path().join("source.png");
        tokio::fs::write(&source_path, &source).await.unwrap();
        let response = serde_json::to_vec(&serde_json::json!({
            "data": [{"b64_json": STANDARD.encode(&source)}]
        }))
        .unwrap();
        let executor = Arc::new(FakeExecutor {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(Some(ImageEditExecutorResponse::new(response))),
        });
        let mut resources = Resources::new();
        resources.insert(ImageEditRuntime::new(
            executor.clone(),
            ImageGenLimits::default(),
        ));
        resources.insert(SessionFolder(tmp.path().to_path_buf()));
        resources.insert(AttachedImages(vec![
            (1, source_path.to_string_lossy().into_owned()),
            (
                3,
                format!("data:image/png;base64,{}", STANDARD.encode(&source)),
            ),
        ]));

        let output = atelier_tool_runtime::Tool::run(
            &ImageEditTool,
            test_ctx_with_call_id(resources.into_shared(), "image-edit-test"),
            ImageEditInput {
                prompt: "combine them".into(),
                image: vec!["[Image #1]".into(), "[Image #3]".into()],
            },
        )
        .await
        .unwrap();

        let requests = executor.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].prompt, "combine them");
        assert_eq!(requests[0].images.len(), 2);
        assert!(
            requests[0]
                .images
                .iter()
                .all(|image| image.mime_type == "image/png")
        );
        assert_eq!(tokio::fs::read(output.0.path).await.unwrap(), source);
    }

    #[tokio::test]
    async fn rejects_stale_or_invented_attachment_references_before_executor_call() {
        let tmp = tempfile::tempdir().unwrap();
        let executor = Arc::new(FakeExecutor {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(None),
        });
        let mut resources = Resources::new();
        resources.insert(ImageEditRuntime::new(
            executor.clone(),
            ImageGenLimits::default(),
        ));
        resources.insert(SessionFolder(tmp.path().to_path_buf()));
        resources.insert(AttachedImages::default());

        let error = atelier_tool_runtime::Tool::run(
            &ImageEditTool,
            test_ctx_with_call_id(resources.into_shared(), "image-edit-test"),
            ImageEditInput {
                prompt: "edit".into(),
                image: vec!["C:/invented/image.png".into()],
            },
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("attachment token"));
        assert!(executor.requests.lock().unwrap().is_empty());
    }
}
