// Per-test-case module for the `pty_e2e_smoke` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// A consumed `/wire-api` subcommand must not become the fuzzy query for the
/// following model stage. This exercises the real composer, SlashController,
/// dropdown renderer, and mock Provider catalog.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wire_api_model_stage_shows_the_full_catalog() {
    let content = ContentController::start_with_models(vec![
        MockModel::new("alpha-model"),
        MockModel::new("grok-imagine-video-preview"),
    ])
    .await
    .expect("start content with multiple models");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/wire-api wire ");

    harness
        .wait_for_text("mock/alpha-model", Duration::from_secs(10))
        .expect("ordinary model remains visible after the wire stage");
    harness
        .wait_for_text("mock/grok-imagine-video-preview", Duration::from_secs(10))
        .expect("second model remains visible after the wire stage");

    let screen = harness.screen_contents();
    assert!(screen.contains("mock/alpha-model"), "{screen}");
    assert!(
        screen.contains("mock/grok-imagine-video-preview"),
        "{screen}"
    );

    harness.quit().expect("clean quit");
}
