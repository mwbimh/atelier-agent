//! Layer 3: Content controller.
//!
//! An idle pager only renders a splash screen — not useful for scroll,
//! stream, or resize scenarios. [`ContentController`] wraps the shared
//! [`MockInferenceServer`] from `atelier-test-support` and provides the
//! isolated Provider configuration that points the bundled shell agent at it,
//! so the pager ends up rendering real agent output.
//!
//! The caller controls the response text via [`ContentController::set_response`].
//! The mock server streams the set response to every inference request.

use std::path::Path;

use anyhow::{Context, Result};
use atelier_test_support::MockInferenceServer;

pub use atelier_test_support::mock_server::LogEntry;
pub use atelier_test_support::mock_server::MockModelEntry as MockModel;
pub use atelier_test_support::mock_server::StorageUpload;
// SSE event builders for `enqueue_response` scripts (reasoning turns etc.).
pub use atelier_test_support::sse;
pub use atelier_test_support::{ScriptedResponse, SseEvent};

/// Drives content into the pager by serving a mock inference endpoint that
/// the bundled shell agent hits for `/v1/chat/completions` and `/v1/responses`.
///
/// Thin wrapper over the shared [`MockInferenceServer`]: adds the isolated
/// `$HOME` sandbox and pager env plumbing, and applies the harness defaults
/// the pager depends on (always-200 `/v1/settings`, fixed default response).
///
/// Shuts the server down on drop (the inner server's `Drop`).
pub struct ContentController {
    server: MockInferenceServer,
    home: tempfile::TempDir,
}

impl ContentController {
    /// Start the mock inference server on a random local port.
    ///
    /// Must be called from within a tokio runtime.
    pub async fn start() -> Result<Self> {
        Self::start_with_models(vec![MockModel::new("test-model")]).await
    }

    /// Start the mock server with a custom set of models returned by
    /// `GET /v1/models`. Use [`MockModel::with_agent_type`] to configure
    /// models with different harness types for agent-type-mismatch tests.
    pub async fn start_with_models(models: Vec<MockModel>) -> Result<Self> {
        let server = MockInferenceServer::start_with_models(models.clone())
            .await
            .context("start mock inference server")?;
        // Pre-delegation parity, both load-bearing for PTY tests: settings
        // must be 200 `{"allow_access": true}` (the shared 404-until-set
        // default strands the pager on the upsell screen), and the response
        // mode must be a fixed text (the shared default is echo).
        server.preset_allow_access();
        server.set_response(default_response_text());

        let home = tempfile::tempdir().context("create temp HOME")?;
        write_mock_provider_config(home.path(), &server.url(), &models)?;

        Ok(Self { server, home })
    }

    /// Base URL of the mock server, e.g. `http://127.0.0.1:41823/v1`.
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Isolated `$HOME` directory that the pager should use (keeps its ~/.atelier
    /// cache/state out of the real home during tests).
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Env vars to pass to the pager process with an isolated Atelier home
    /// and unrelated background services disabled.
    ///
    /// Mirrors `atelier_test_support::env::test_env_cmd_tokio`.
    pub fn env_for_pager(&self) -> Vec<(String, String)> {
        let home = self.home.path().to_string_lossy().into_owned();
        let atelier_home = self
            .home
            .path()
            .join(".atelier")
            .to_string_lossy()
            .into_owned();
        vec![
            ("HOME".into(), home),
            // Explicit ATELIER_HOME prevents leaking the real user's
            // config.toml when $HOME alone isn't sufficient (e.g. if
            // ATELIER_HOME is set in the test runner's env).
            ("ATELIER_HOME".into(), atelier_home),
            ("ATELIER_TELEMETRY_ENABLED".into(), "false".into()),
            ("ATELIER_FEEDBACK_ENABLED".into(), "false".into()),
            // Next-prompt autocomplete fires an extra background model call
            // at every turn end (default ON). Off by default in PTY tests so
            // the mock's fixed response can't leak in as ghost text and
            // scripted per-path FIFOs aren't consumed by it. Tests exercising
            // the feature re-enable it via extra env.
            ("ATELIER_PROMPT_SUGGESTIONS".into(), "false".into()),
            // No inference retries in tests. The mock always answers 200, so a
            // retry only ever fires when a turn is deliberately stalled
            // (`hold_agent_completions` / a long `chunk_delay`). On a slow
            // runner that stall can exceed the client's first-token budget and
            // retry the request — and because the mock serves `set_agent_turns`
            // by popping one response per REQUEST, a retry consumes the next
            // turn's slot, misaligning every following turn (the promoted queue
            // prompt then hangs waiting for a response that was already popped).
            // Pinning retries to 0 keeps one request == one turn.
            ("ATELIER_MAX_RETRIES".into(), "0".into()),
        ]
    }

    /// Replace the mocked assistant response. All subsequent chat completion
    /// requests will stream this text word-by-word.
    pub fn set_response(&self, text: impl Into<String>) {
        self.server.set_response(text);
    }

    /// Queue a byte-exact scripted response for the next request on `path`
    /// (e.g. `"/v1/responses"`). Consumed FIFO per path; falls back to the
    /// active fixed/echo mode when the queue is empty.
    pub fn enqueue_response(&self, path: impl Into<String>, response: ScriptedResponse) {
        self.server.enqueue_response(path, response);
    }

    /// Access the underlying mock inference server for advanced scripting.
    pub fn server(&self) -> &MockInferenceServer {
        &self.server
    }

    /// Pace the mocked SSE streams: each event is emitted after `delay`.
    /// `None` restores instant streaming. Use to hold a turn visibly
    /// "streaming" long enough to interact with it (e.g. Esc-cancel tests).
    pub fn set_chunk_delay(&self, delay: Option<std::time::Duration>) {
        self.server.set_chunk_delay(delay);
    }

    /// Hold every agent turn's completion until [`release_agent_completions`]
    /// is called. Keeps a turn deterministically "streaming" so a test can
    /// interact with it (queue edits/removals) without racing turn end.
    ///
    /// [`release_agent_completions`]: Self::release_agent_completions
    pub fn hold_agent_completions(&self) {
        self.server.hold_agent_completions();
    }

    /// Release a hold set by [`hold_agent_completions`], letting the gated
    /// turn complete.
    ///
    /// [`hold_agent_completions`]: Self::hold_agent_completions
    pub fn release_agent_completions(&self) {
        self.server.release_agent_completions();
    }

    /// Queue one response per agent turn (FIFO) so each carries a distinct
    /// sentinel. See [`MockInferenceServer::set_agent_turns`].
    pub fn set_turns(&self, turns: impl IntoIterator<Item = String>) {
        self.server.set_agent_turns(turns);
    }

    /// Number of inference requests the pager has made so far.
    pub fn request_count(&self) -> u32 {
        self.server.request_count()
    }

    /// Whether the server has seen a chat completion request.
    pub fn has_chat_completion(&self) -> bool {
        self.server.has_chat_completion_request() || self.server.has_responses_request()
    }

    /// Snapshot of all received requests — useful for test diagnostics.
    pub fn requests(&self) -> Vec<LogEntry> {
        self.server.requests()
    }

    pub fn request_bodies(&self) -> Vec<serde_json::Value> {
        self.server.request_bodies()
    }

    // ── Mock storage controls (park-on-401 e2e) ────────────────────────────

    /// Flip the mock `/v1/storage` 401 gate (the auth-outage window).
    pub fn set_storage_unauthorized(&self, unauthorized: bool) {
        self.server.set_storage_unauthorized(unauthorized);
    }

    /// Total `/v1/storage` upload attempts, including 401-rejected ones.
    pub fn storage_request_count(&self) -> u32 {
        self.server.storage_request_count()
    }

    /// Snapshot of accepted (HTTP 200) `/v1/storage` uploads.
    pub fn storage_uploads(&self) -> Vec<StorageUpload> {
        self.server.storage_uploads()
    }
}

fn write_mock_provider_config(home: &Path, base_url: &str, models: &[MockModel]) -> Result<()> {
    let default_model = models
        .first()
        .context("PTY mock Provider requires at least one model")?;
    let atelier_home = home.join(".atelier");
    let provider_models = atelier_home.join("models/providers/mock");
    std::fs::create_dir_all(&provider_models).context("create mock Provider config directories")?;

    let quoted_base_url = serde_json::to_string(base_url).context("quote mock Provider URL")?;
    let providers = format!(
        r#"schema_version = 3

[providers.mock]
display_name = "PTY mock"
auth = {{ type = "none" }}
base_url = {quoted_base_url}
enabled = true

[providers.mock.credential]
type = "none"

[providers.mock.discovery]
type = "open_ai_models"
path = "models"
"#,
    );
    std::fs::write(atelier_home.join("providers.toml"), providers)
        .context("write mock providers.toml")?;

    let mut model_config = String::from("schema_version = 1\n");
    for model in models {
        let quoted_id = serde_json::to_string(&model.id).context("quote mock model ID")?;
        let wire_api = match model.api_backend.as_deref() {
            Some("responses") => "responses",
            Some("messages") => "messages",
            _ => "chat_completions",
        };
        model_config.push_str(&format!(
            r#"
[models.{quoted_id}]
wire_api = "{wire_api}"
context_window = 128000

[models.{quoted_id}.capabilities]
tool_calls = true
parallel_tool_calls = true
"#,
        ));
    }
    std::fs::write(provider_models.join("models.toml"), model_config)
        .context("write mock Provider models.toml")?;

    let quoted_default_model =
        serde_json::to_string(&default_model.id).context("quote default mock model ID")?;
    let mut roles = String::from("schema_version = 1\n");
    for role in [
        "main",
        "explore",
        "implement",
        "review",
        "test",
        "compact",
        "summary",
        "title",
        "planner",
        "strategist",
        "skeptic",
    ] {
        roles.push_str(&format!(
            r#"
[roles.{role}]
provider = "mock"
model = {quoted_default_model}
"#,
        ));
    }
    std::fs::write(atelier_home.join("roles.toml"), roles).context("write mock roles.toml")?;

    std::fs::write(
        atelier_home.join("config.toml"),
        r#"context = "default"
request_agent = "atelier"

sandbox = { profile = "off", backend = "unsafe" }
"#,
    )
    .context("write mock config.toml")?;

    Ok(())
}

fn default_response_text() -> String {
    "Hello from the pty_harness mock inference server.".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-delegation mock always served 200 `{"allow_access": true}`;
    /// the shared server defaults to 404-until-set. A 404 strands the pager
    /// on the SuperAtelier upsell screen and breaks every PTY test.
    #[tokio::test]
    async fn settings_endpoint_allows_access_by_default() {
        let content = ContentController::start().await.unwrap();

        let resp = reqwest::get(format!("{}/settings", content.url()))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body, serde_json::json!({ "allow_access": true }));
    }

    /// The pre-delegation mock streamed a fixed default text to every
    /// request; the shared server defaults to echo.
    #[tokio::test]
    async fn default_response_streams_fixed_text() {
        let content = ContentController::start().await.unwrap();

        let body = reqwest::Client::new()
            .post(format!("{}/chat/completions", content.url()))
            .json(&serde_json::json!({
                "model": "test-model",
                "messages": [{ "role": "user", "content": "anything" }]
            }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        let streamed: String = body
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim_start)
            .filter(|d| *d != "[DONE]")
            .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
            .filter_map(|v| {
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
            .collect();
        assert_eq!(streamed, default_response_text());
        assert!(content.has_chat_completion());
    }

    /// `env_for_pager` keeps the exact sandbox env contract the pager spawn
    /// path depends on without installing a privileged Provider fallback.
    #[tokio::test]
    async fn env_for_pager_shape() {
        let content = ContentController::start().await.unwrap();
        let env = content.env_for_pager();
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key.as_str() == k)
                .map(|(_, v)| v.clone())
        };

        assert_eq!(get("HOME").as_deref(), content.home().to_str());
        assert_eq!(
            get("ATELIER_HOME").as_deref(),
            content.home().join(".atelier").to_str()
        );
        assert_eq!(get("ATELIER_CLI_CHAT_PROXY_BASE_URL"), None);
        assert_eq!(get("ATELIER_XAI_API_BASE_URL"), None);
        assert_eq!(get("XAI_API_KEY"), None);
        assert_eq!(get("ATELIER_TELEMETRY_ENABLED").as_deref(), Some("false"));
        assert_eq!(get("ATELIER_FEEDBACK_ENABLED").as_deref(), Some("false"));
        assert_eq!(get("ATELIER_PROMPT_SUGGESTIONS").as_deref(), Some("false"));
        assert_eq!(get("ATELIER_MAX_RETRIES").as_deref(), Some("0"));
        assert_eq!(env.len(), 6, "env list must not silently grow or shrink");

        let atelier_home = content.home().join(".atelier");
        let config = std::fs::read_to_string(atelier_home.join("config.toml")).unwrap();
        assert!(config.contains("sandbox = { profile = \"off\", backend = \"unsafe\" }"));

        let providers = std::fs::read_to_string(atelier_home.join("providers.toml")).unwrap();
        assert!(providers.contains("[providers.mock]"));
        assert!(providers.contains(&format!("base_url = {:?}", content.url())));
        assert!(providers.contains("type = \"none\""));

        let models =
            std::fs::read_to_string(atelier_home.join("models/providers/mock/models.toml"))
                .unwrap();
        assert!(models.contains("[models.\"test-model\"]"));
        assert!(models.contains("tool_calls = true"));

        let roles = std::fs::read_to_string(atelier_home.join("roles.toml")).unwrap();
        assert!(roles.contains("[roles.main]"));
        assert!(roles.contains("provider = \"mock\""));
        assert!(roles.contains("model = \"test-model\""));
    }
}
