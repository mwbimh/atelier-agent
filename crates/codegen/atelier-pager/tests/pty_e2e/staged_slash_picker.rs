// Per-test-case module for the `pty_e2e_smoke` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// A consumed `/wire-api` subcommand must not become the fuzzy query for the
/// following model stage, while non-inference media models remain filtered.
/// This exercises the real composer, SlashController, dropdown renderer, and
/// mock Provider catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wire_api_set_model_stage_shows_only_inference_models() {
    let content = ContentController::start_with_models(vec![
        MockModel::new("alpha-model"),
        MockModel::new("grok-imagine-video-preview"),
    ])
    .await
    .expect("start content with multiple models");
    let model_config_path = content
        .home()
        .join(".atelier/models/providers/mock/models.toml");
    let model_config = std::fs::read_to_string(&model_config_path)
        .expect("read mock Provider model purposes")
        .replace(
            "[models.\"grok-imagine-video-preview\"]\npurpose = \"inference\"\nwire_api = \"chat_completions\"",
            "[models.\"grok-imagine-video-preview\"]\npurpose = \"video_generation\"",
        );
    std::fs::write(&model_config_path, model_config)
        .expect("mark the mock video model as non-inference");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/wire-api set ");

    harness
        .wait_for_text("mock/alpha-model", Duration::from_secs(10))
        .expect("ordinary model remains visible after the wire stage");
    let screen = harness.screen_contents();
    assert!(screen.contains("mock/alpha-model"), "{screen}");
    assert!(
        !screen.contains("mock/grok-imagine-video-preview"),
        "{screen}"
    );

    harness.quit().expect("clean quit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn provider_delete_uses_a_default_safe_confirmation_modal() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/provider delete mock");
    harness
        .inject_keys(b"\r")
        .expect("open delete confirmation");

    harness
        .wait_for_text("Delete Provider?", Duration::from_secs(10))
        .expect("delete confirmation title");
    harness
        .wait_for_text("Keep the current configuration", Duration::from_secs(10))
        .expect("safe default description");
    let screen = harness.screen_contents();
    assert!(screen.contains("Delete Provider 'mock'?"), "{screen}");
    assert!(screen.contains("Cancel"), "{screen}");
    assert!(!screen.contains("delete mock confirm"), "{screen}");

    harness
        .inject_keys(b"\r")
        .expect("Enter activates the default Cancel choice");
    wait_for_labels_absent(
        &mut harness,
        &["Keep the current configuration"],
        Duration::from_secs(10),
    );
    assert!(
        !harness
            .screen_contents()
            .contains("Keep the current configuration"),
        "confirmation should close after the safe default is selected"
    );

    harness.quit().expect("clean quit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wire_api_reset_uses_reset_language_and_confirmation_modal() {
    let content = ContentController::start().await.expect("start content");
    let provider_path = content.home().join(".atelier").join("providers.toml");
    let mut registry = atelier_provider::ProviderRegistry::load_or_create(provider_path)
        .expect("load mock Provider registry");
    let model_key = atelier_provider::ModelKey::new("mock", "test-model").unwrap();
    registry
        .set_model_provider_override(
            &model_key,
            atelier_provider::ProviderModelOverride {
                wire_api: Some(atelier_provider::WireApi::Messages),
                payload: serde_json::Map::new(),
            },
        )
        .expect("seed exact Wire API override");
    registry.save().expect("persist exact Wire API override");

    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/wire-api reset ");
    harness
        .wait_for_text("mock/test-model", Duration::from_secs(10))
        .expect("exact override appears in reset picker");
    let picker = harness.screen_contents();
    assert!(!picker.contains("mock/alpha-model"), "{picker}");
    assert!(
        !picker.contains("mock/grok-imagine-video-preview"),
        "{picker}"
    );
    harness
        .inject_keys(b"\x15")
        .expect("clear reset picker input");

    inject_keys_paced(&mut harness, b"/wire-api reset mock/test-model");
    harness.inject_keys(b"\r").expect("open reset confirmation");

    harness
        .wait_for_text("Reset Wire API override?", Duration::from_secs(10))
        .expect("reset confirmation title");
    harness
        .wait_for_text(
            "return to its inherited definition",
            Duration::from_secs(10),
        )
        .expect("reset consequence");
    harness
        .wait_for_text("After: chat_completions", Duration::from_secs(10))
        .expect("reset confirmation shows the inherited protocol");
    let screen = harness.screen_contents();
    assert!(screen.contains("Reset override"), "{screen}");
    assert!(
        !screen.contains("delete mock/test-model confirm"),
        "{screen}"
    );

    harness.inject_keys(b"\x1b").expect("Esc cancels reset");
    harness.quit().expect("clean quit");
}
