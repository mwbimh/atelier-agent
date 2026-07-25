//! Minimal provider-agnostic text-to-image tool.
//!
//! `atelier-tools` owns validation and local persistence only. Network access
//! is delegated to an injected [`ImageGenExecutor`], allowing the sampler-side
//! adapter to provide transport later without giving this crate another HTTP
//! path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::types::output::{MediaGenOutput, ToolOutput};
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::SessionFolder;
use crate::types::tool::{ToolKind, ToolNamespace};

pub const IMAGE_GEN_TOOL_NAME: &str = "image_gen";
pub const IMAGINE_COMMAND_NAME: &str = "imagine";
const IMAGE_GEN_TOOL_ID: &str = "AtelierBuild:image_gen";
const IMAGES_DIR: &str = "images";

const DEFAULT_MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_BASE64_BYTES: usize = 24 * 1024 * 1024;
const DEFAULT_MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_PIXELS: u64 = 64_000_000;

pub fn imagine_usage_message() -> &'static str {
    "Usage: /imagine <description>"
}

pub fn imagine_instruction(prompt: &str) -> String {
    format!("Call the `{IMAGE_GEN_TOOL_NAME}` tool once with exactly this prompt: {prompt}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageGenLimits {
    pub max_response_bytes: usize,
    pub max_base64_bytes: usize,
    pub max_file_bytes: usize,
    pub max_pixels: u64,
}

impl Default for ImageGenLimits {
    fn default() -> Self {
        Self {
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_base64_bytes: DEFAULT_MAX_BASE64_BYTES,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_pixels: DEFAULT_MAX_PIXELS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenRequest {
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub struct ImageGenExecutorResponse {
    body: Vec<u8>,
}

impl ImageGenExecutorResponse {
    pub fn new(body: impl Into<Vec<u8>>) -> Self {
        Self { body: body.into() }
    }

    fn body(&self) -> &[u8] {
        &self.body
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct ImageGenExecutorError {
    message: String,
}

impl ImageGenExecutorError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[async_trait::async_trait]
pub trait ImageGenExecutor: Send + Sync + 'static {
    async fn execute(
        &self,
        request: ImageGenRequest,
    ) -> Result<ImageGenExecutorResponse, ImageGenExecutorError>;
}

#[derive(Clone)]
pub enum ImageGenConfig {
    Disabled,
    /// Compatibility-only shape retained for existing shell call sites. It
    /// does not enable the tool because no executor is present.
    Enabled {
        api_key: String,
        base_url: String,
        extra_headers: indexmap::IndexMap<String, String>,
        image_gen_enabled: bool,
        image_edit_enabled: bool,
        model_override: Option<String>,
        tier_restricted: bool,
    },
    Executor {
        executor: Arc<dyn ImageGenExecutor>,
        limits: ImageGenLimits,
    },
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self::Disabled
    }
}

impl std::fmt::Debug for ImageGenConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => formatter.write_str("ImageGenConfig::Disabled"),
            Self::Enabled { .. } => formatter
                .debug_struct("ImageGenConfig::Enabled")
                .field("compatibility_only", &true)
                .finish(),
            Self::Executor { limits, .. } => formatter
                .debug_struct("ImageGenConfig::Executor")
                .field("limits", limits)
                .finish_non_exhaustive(),
        }
    }
}

impl ImageGenConfig {
    pub fn enabled(executor: Arc<dyn ImageGenExecutor>) -> Self {
        Self::Executor {
            executor,
            limits: ImageGenLimits::default(),
        }
    }

    pub fn enabled_with_limits(
        executor: Arc<dyn ImageGenExecutor>,
        limits: ImageGenLimits,
    ) -> Self {
        Self::Executor { executor, limits }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Executor { .. })
    }

    pub fn has_credentials(&self) -> bool {
        self.is_enabled()
    }

    pub fn image_gen_enabled(&self) -> bool {
        self.is_enabled()
    }

    pub fn image_edit_enabled(&self) -> bool {
        false
    }

    pub fn model_override(&self) -> Option<&str> {
        None
    }

    pub(crate) fn runtime(&self) -> Option<ImageGenRuntime> {
        match self {
            Self::Executor { executor, limits } => {
                Some(ImageGenRuntime::new(executor.clone(), *limits))
            }
            Self::Disabled | Self::Enabled { .. } => None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ImageGenRuntime {
    executor: Arc<dyn ImageGenExecutor>,
    limits: ImageGenLimits,
}

impl ImageGenRuntime {
    pub(crate) fn new(executor: Arc<dyn ImageGenExecutor>, limits: ImageGenLimits) -> Self {
        Self { executor, limits }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageGenInput {
    /// Text description of the image to generate. Passed to the executor verbatim.
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageGenToolOutput(pub MediaGenOutput);

impl atelier_tool_runtime::ToolOutput for ImageGenToolOutput {}

impl From<ImageGenToolOutput> for ToolOutput {
    fn from(output: ImageGenToolOutput) -> Self {
        Self::ImageGen(output.0)
    }
}

#[derive(Debug, Default)]
pub struct ImageGenTool;

impl crate::types::tool_metadata::ToolMetadata for ImageGenTool {
    fn kind(&self) -> ToolKind {
        ToolKind::ImageGen
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::AtelierBuild
    }

    fn description_template(&self) -> &str {
        "Generate one image from a text prompt and save it in the current session's images directory."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl atelier_tool_runtime::Tool for ImageGenTool {
    type Args = ImageGenInput;
    type Output = ImageGenToolOutput;

    fn id(&self) -> atelier_tool_protocol::ToolId {
        atelier_tool_protocol::ToolId::new(IMAGE_GEN_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &atelier_tool_runtime::ListToolsContext,
    ) -> atelier_tool_types::ToolDescription {
        atelier_tool_types::ToolDescription::new(
            IMAGE_GEN_TOOL_NAME,
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

    #[tracing::instrument(name = "tool.image_gen", skip_all)]
    async fn run(
        &self,
        ctx: atelier_tool_runtime::ToolCallContext,
        input: ImageGenInput,
    ) -> Result<ImageGenToolOutput, atelier_tool_runtime::ToolError> {
        if input.prompt.trim().is_empty() {
            return Err(tool_error("invalid_prompt", "prompt must not be empty"));
        }

        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let (runtime, session_folder) = {
            let resources = resources.lock().await;
            (
                resources.require::<ImageGenRuntime>()?.clone(),
                resources.require::<SessionFolder>()?.0.clone(),
            )
        };

        let response = runtime
            .executor
            .execute(ImageGenRequest {
                prompt: input.prompt,
            })
            .await
            .map_err(|_| {
                tool_error(
                    "image_generation_failed",
                    "image generation executor failed",
                )
            })?;
        let image = decode_response(response.body(), runtime.limits)?;
        let path = save_image_atomically(&session_folder, &image.bytes, image.extension).await?;
        Ok(ImageGenToolOutput(MediaGenOutput::new(path)))
    }
}

#[derive(Deserialize)]
struct ImageResponse {
    #[serde(default)]
    data: Vec<ImageResponseItem>,
}

#[derive(Deserialize)]
struct ImageResponseItem {
    #[serde(default)]
    b64_json: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    url: Option<String>,
}

struct DecodedImage {
    bytes: Vec<u8>,
    extension: &'static str,
}

fn decode_response(
    body: &[u8],
    limits: ImageGenLimits,
) -> Result<DecodedImage, atelier_tool_runtime::ToolError> {
    if body.len() > limits.max_response_bytes {
        return Err(tool_error(
            "image_response_too_large",
            format!(
                "image response size exceeds {} bytes",
                limits.max_response_bytes
            ),
        ));
    }
    let response: ImageResponse = serde_json::from_slice(body).map_err(|_| {
        tool_error(
            "invalid_image_response",
            "image executor returned invalid JSON",
        )
    })?;
    let encoded = response
        .data
        .into_iter()
        .find_map(|item| item.b64_json)
        .ok_or_else(|| {
            tool_error(
                "missing_base64_image",
                "image response did not contain a base64 image",
            )
        })?;
    if encoded.len() > limits.max_base64_bytes {
        return Err(tool_error(
            "image_base64_too_large",
            format!(
                "image base64 size exceeds {} bytes",
                limits.max_base64_bytes
            ),
        ));
    }
    let bytes = STANDARD.decode(encoded.as_bytes()).map_err(|_| {
        tool_error(
            "invalid_image_base64",
            "image response contained invalid base64",
        )
    })?;
    if bytes.len() > limits.max_file_bytes {
        return Err(tool_error(
            "image_file_too_large",
            format!("image file size exceeds {} bytes", limits.max_file_bytes),
        ));
    }

    let (width, height, mime) =
        crate::util::image_validate::validate_image_bytes_with(&bytes, false)
            .map_err(|_| tool_error("invalid_image", "base64 payload is not a valid image"))?;
    if u64::from(width).saturating_mul(u64::from(height)) > limits.max_pixels {
        return Err(tool_error(
            "image_dimensions_too_large",
            format!("image exceeds {} decoded pixels", limits.max_pixels),
        ));
    }
    crate::util::image_validate::validate_image_bytes(&bytes)
        .map_err(|_| tool_error("invalid_image", "base64 payload is not a valid image"))?;

    let extension = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => {
            return Err(tool_error(
                "unsupported_image_format",
                "base64 payload uses an unsupported image format",
            ));
        }
    };
    Ok(DecodedImage { bytes, extension })
}

async fn save_image_atomically(
    session_folder: &Path,
    bytes: &[u8],
    extension: &'static str,
) -> Result<PathBuf, atelier_tool_runtime::ToolError> {
    let session_folder = session_folder.to_path_buf();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&session_folder)
            .map_err(|_| tool_error("image_write_failed", "failed to create session directory"))?;
        let canonical_session = dunce::canonicalize(&session_folder)
            .map_err(|_| tool_error("image_write_failed", "failed to resolve session directory"))?;
        let images_dir = canonical_session.join(IMAGES_DIR);
        if images_dir.exists() {
            let metadata = std::fs::symlink_metadata(&images_dir).map_err(|_| {
                tool_error("image_write_failed", "failed to inspect images directory")
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(tool_error(
                    "image_path_escape",
                    "session images path is not a regular directory",
                ));
            }
        } else {
            std::fs::create_dir(&images_dir).map_err(|_| {
                tool_error("image_write_failed", "failed to create images directory")
            })?;
        }
        let canonical_images = dunce::canonicalize(&images_dir)
            .map_err(|_| tool_error("image_write_failed", "failed to resolve images directory"))?;
        ensure_images_directory_is_direct_child(&canonical_session, &canonical_images)?;

        let filename = format!("{}.{}", uuid::Uuid::now_v7(), extension);
        let destination = canonical_images.join(filename);
        if destination.parent() != Some(canonical_images.as_path()) {
            return Err(tool_error(
                "image_path_escape",
                "generated image path escapes the images directory",
            ));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&canonical_images)
            .map_err(|_| tool_error("image_write_failed", "failed to create image temp file"))?;
        use std::io::Write;
        temporary
            .write_all(&bytes)
            .map_err(|_| tool_error("image_write_failed", "failed to write image temp file"))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| tool_error("image_write_failed", "failed to sync image temp file"))?;
        temporary
            .persist_noclobber(&destination)
            .map_err(|_| tool_error("image_write_failed", "failed to atomically persist image"))?;
        Ok(destination)
    })
    .await
    .map_err(|_| tool_error("image_write_failed", "image writer task failed"))?
}

fn ensure_images_directory_is_direct_child(
    canonical_session: &Path,
    canonical_images: &Path,
) -> Result<(), atelier_tool_runtime::ToolError> {
    if !canonical_images.starts_with(canonical_session)
        || canonical_images.parent() != Some(canonical_session)
    {
        return Err(tool_error(
            "image_path_escape",
            "session images directory escapes the session root",
        ));
    }
    Ok(())
}

fn tool_error(code: &'static str, message: impl Into<String>) -> atelier_tool_runtime::ToolError {
    atelier_tool_runtime::ToolError::custom(code, message.into())
}

pub(crate) fn is_image_gen_tool_id(id: &str) -> bool {
    id == IMAGE_GEN_TOOL_ID
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::types::output::MediaGenOutput;
    use crate::types::resources::{Resources, SessionFolder};
    use crate::types::tool_metadata::test_ctx_with_call_id;

    #[derive(Debug, Default)]
    struct FakeExecutor {
        requests: Mutex<Vec<ImageGenRequest>>,
        response: Mutex<Option<Result<ImageGenExecutorResponse, ImageGenExecutorError>>>,
    }

    impl FakeExecutor {
        fn returning(body: impl Into<Vec<u8>>) -> Arc<Self> {
            Arc::new(Self {
                requests: Mutex::new(Vec::new()),
                response: Mutex::new(Some(Ok(ImageGenExecutorResponse::new(body)))),
            })
        }
    }

    #[async_trait::async_trait]
    impl ImageGenExecutor for FakeExecutor {
        async fn execute(
            &self,
            request: ImageGenRequest,
        ) -> Result<ImageGenExecutorResponse, ImageGenExecutorError> {
            self.requests.lock().unwrap().push(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .expect("fake response configured")
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

    fn response_with_base64(bytes: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "data": [{"b64_json": STANDARD.encode(bytes)}]
        }))
        .unwrap()
    }

    async fn run_tool(
        executor: Arc<dyn ImageGenExecutor>,
        limits: ImageGenLimits,
        session_folder: &std::path::Path,
        prompt: &str,
    ) -> Result<ImageGenToolOutput, atelier_tool_runtime::ToolError> {
        let mut resources = Resources::new();
        resources.insert(ImageGenRuntime::new(executor, limits));
        resources.insert(SessionFolder(session_folder.to_path_buf()));
        atelier_tool_runtime::Tool::run(
            &ImageGenTool,
            test_ctx_with_call_id(resources.into_shared(), "image-gen-test"),
            ImageGenInput {
                prompt: prompt.to_owned(),
            },
        )
        .await
    }

    #[test]
    fn compatibility_config_is_always_fail_closed() {
        let configured = ImageGenConfig::Enabled {
            api_key: "unused".into(),
            base_url: "https://example.invalid".into(),
            extra_headers: Default::default(),
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: Some("unused".into()),
            tier_restricted: false,
        };
        assert!(!configured.has_credentials());
        assert!(!configured.image_gen_enabled());
        assert!(!configured.image_edit_enabled());
        assert_eq!(configured.model_override(), None);
    }

    #[tokio::test]
    async fn fake_executor_receives_prompt_and_writes_typed_media_path() {
        let tmp = tempfile::tempdir().unwrap();
        let image = png_bytes();
        let executor = FakeExecutor::returning(response_with_base64(&image));

        let output = run_tool(
            executor.clone(),
            ImageGenLimits::default(),
            tmp.path(),
            "a blue glass fox",
        )
        .await
        .unwrap();

        assert_eq!(
            executor.requests.lock().unwrap().as_slice(),
            &[ImageGenRequest {
                prompt: "a blue glass fox".to_owned()
            }]
        );
        let MediaGenOutput {
            path,
            filename,
            session_folder,
            uploaded_url,
        } = output.0;
        assert_eq!(session_folder, "images");
        assert!(filename.ends_with(".png"));
        assert_eq!(uploaded_url, None);
        assert_eq!(path.parent().unwrap(), tmp.path().join("images"));
        assert_eq!(tokio::fs::read(path).await.unwrap(), image);
    }

    #[tokio::test]
    async fn rejects_url_only_response_without_creating_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::returning(
            br#"{"data":[{"url":"https://images.example.invalid/result.png"}]}"#.to_vec(),
        );

        let error = run_tool(
            executor,
            ImageGenLimits::default(),
            tmp.path(),
            "do not fetch",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("base64"));
        assert!(!tmp.path().join("images").exists());
    }

    #[tokio::test]
    async fn enforces_response_size_limit_before_json_parsing() {
        let tmp = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::returning(vec![b'x'; 33]);
        let limits = ImageGenLimits {
            max_response_bytes: 32,
            ..ImageGenLimits::default()
        };

        let error = run_tool(executor, limits, tmp.path(), "oversized response")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("response size"));
    }

    #[tokio::test]
    async fn enforces_base64_size_limit_before_decoding() {
        let tmp = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::returning(response_with_base64(&png_bytes()));
        let limits = ImageGenLimits {
            max_base64_bytes: 8,
            ..ImageGenLimits::default()
        };

        let error = run_tool(executor, limits, tmp.path(), "oversized base64")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("base64 size"));
    }

    #[tokio::test]
    async fn enforces_file_size_limit_after_decoding() {
        let tmp = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::returning(response_with_base64(&png_bytes()));
        let limits = ImageGenLimits {
            max_file_bytes: 8,
            ..ImageGenLimits::default()
        };

        let error = run_tool(executor, limits, tmp.path(), "oversized file")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("file size"));
        assert!(!tmp.path().join("images").exists());
    }

    #[tokio::test]
    async fn rejects_non_image_base64_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let executor = FakeExecutor::returning(response_with_base64(b"not an image"));

        let error = run_tool(
            executor,
            ImageGenLimits::default(),
            tmp.path(),
            "invalid bytes",
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("valid image"));
        assert!(!tmp.path().join("images").exists());
    }

    #[test]
    fn rejects_canonical_images_directory_outside_session_root() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let canonical_session = dunce::canonicalize(session).unwrap();
        let canonical_outside = dunce::canonicalize(outside).unwrap();

        let error = ensure_images_directory_is_direct_child(&canonical_session, &canonical_outside)
            .unwrap_err();

        assert!(error.to_string().contains("escapes the session root"));
    }

    #[tokio::test]
    async fn rejects_images_directory_symlink_escape_when_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("session");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, session.join("images")).unwrap();

        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_dir(&outside, session.join("images")) {
            const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
            {
                return;
            }
            panic!("failed to create test directory symlink: {error}");
        }

        let executor = FakeExecutor::returning(response_with_base64(&png_bytes()));
        let error = run_tool(
            executor,
            ImageGenLimits::default(),
            &session,
            "must stay inside session",
        )
        .await
        .unwrap_err();

        assert!(
            error.to_string().contains("regular directory")
                || error.to_string().contains("escapes the session root")
        );
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }
}
