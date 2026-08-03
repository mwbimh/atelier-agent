// Per-test-case module for the `pty_e2e_smoke` integration test crate.
#[allow(unused_imports)]
use super::common::*;

/// `/wire-api` must open a real protocol picker instead of showing only a hint.
/// This exercises the composer, SlashController, and dropdown renderer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wire_api_shows_the_simple_protocol_picker() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/wire-api ");

    harness
        .wait_for_text("responses", Duration::from_secs(10))
        .expect("responses picker entry");
    let screen = harness.screen_contents();
    assert!(screen.contains("responses"), "{screen}");
    assert!(screen.contains("message"), "{screen}");
    assert!(screen.contains("chat"), "{screen}");
    assert!(!screen.contains("payload"), "{screen}");
    assert!(!screen.contains("reset"), "{screen}");

    harness.quit().expect("clean quit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wire_api_updates_and_persists_only_the_current_pair() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/wire-api message");
    harness.inject_keys(b"\r").expect("submit Wire API change");
    harness
        .wait_for_text("Wire API updated", Duration::from_secs(10))
        .expect("Wire API update confirmation");
    harness.quit().expect("clean quit");

    let provider_path = content.home().join(".atelier").join("providers.toml");
    let registry = atelier_provider::ProviderRegistry::load_or_create(provider_path)
        .expect("reload persisted Provider registry");
    let model_key = atelier_provider::ModelKey::new("mock", "test-model").unwrap();
    let exact = registry
        .model_provider_override(&model_key)
        .expect("current pair has an exact override");
    assert_eq!(exact.wire_api, Some(atelier_provider::WireApi::Messages));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn provider_shows_only_the_four_simple_actions() {
    let content = ContentController::start().await.expect("start content");
    let binary = pager_binary().expect("resolve pager binary");
    let mut harness =
        PtyHarness::spawn_with_content(&binary, DEFAULT_ROWS, DEFAULT_COLS, &content, &[])
            .expect("spawn pager with content");
    harness.set_respond_to_queries(true);

    harness
        .wait_for_text(WELCOME_SCREEN_SENTINEL, WELCOME_TIMEOUT)
        .expect("welcome text");
    inject_keys_paced(&mut harness, b"/provider ");
    harness
        .wait_for_text("List configured Providers", Duration::from_secs(10))
        .expect("Provider action picker");
    let screen = harness.screen_contents();
    for action in ["list", "add", "delete", "refresh"] {
        assert!(screen.contains(action), "missing {action}: {screen}");
    }
    for removed in ["edit", "enable", "disable", "test", "login", "logout"] {
        assert!(
            !screen.contains(&format!("\n       {removed}")),
            "unexpected {removed}: {screen}"
        );
    }

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
